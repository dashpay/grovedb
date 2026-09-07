//! Tests for batch operation rejection of internal-only ops.
//!
//! Internal-only operations (`ReplaceTreeRootKey`, `InsertTreeWithRootHash`,
//! `InsertNonMerkTree`) are produced exclusively by propagation or
//! preprocessing. They must never be accepted when submitted directly via
//! `apply_batch`.
//!
//! Two layers enforce this:
//! 1. `verify_consistency_of_operations` (consistency check) catches them first
//!    with "batch operations fail consistency checks".
//! 2. `from_ops` in `batch_structure.rs` provides defense-in-depth with
//!    "replace and insert tree hash are internal operations only".
//!
//! These tests verify layer 1 (the consistency check), which is the first
//! guard that fires at the `apply_batch` entry point.

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_merk::tree::AggregateData;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{
        key_info::KeyInfo::KnownKey, GroveOp, KeyInfoPath, NonMerkTreeMeta, QualifiedGroveDbOp,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error,
};

#[test]
fn test_apply_batch_rejects_replace_tree_root_key() {
    let db = make_empty_grovedb();
    let grove_version = GroveVersion::latest();

    let op = QualifiedGroveDbOp {
        path: KeyInfoPath(vec![]),
        key: Some(KnownKey(b"test".to_vec())),
        op: GroveOp::ReplaceTreeRootKey {
            hash: [0u8; 32],
            root_key: None,
            aggregate_data: AggregateData::NoAggregateData,
        },
    };

    let result = db.apply_batch(vec![op], None, None, grove_version).value;

    match result {
        Err(Error::InvalidBatchOperation(_)) => {
            // Correctly rejected -- the consistency check fires first with
            // "batch operations fail consistency checks", blocking the
            // internal-only ReplaceTreeRootKey op.
        }
        Err(other) => {
            panic!(
                "expected InvalidBatchOperation error, got different error: {:?}",
                other
            );
        }
        Ok(()) => {
            panic!("expected InvalidBatchOperation error, but apply_batch succeeded");
        }
    }
}

#[test]
fn test_apply_batch_rejects_insert_tree_with_root_hash() {
    let db = make_empty_grovedb();
    let grove_version = GroveVersion::latest();

    let op = QualifiedGroveDbOp {
        path: KeyInfoPath(vec![]),
        key: Some(KnownKey(b"test".to_vec())),
        op: GroveOp::InsertTreeWithRootHash {
            hash: [0u8; 32],
            root_key: None,
            flags: None,
            aggregate_data: AggregateData::NoAggregateData,

            non_counted: false,
            not_summed: false,
            not_counted_or_summed: false,
        },
    };

    let result = db.apply_batch(vec![op], None, None, grove_version).value;

    match result {
        Err(Error::InvalidBatchOperation(_)) => {
            // Correctly rejected -- the consistency check fires first with
            // "batch operations fail consistency checks", blocking the
            // internal-only InsertTreeWithRootHash op.
        }
        Err(other) => {
            panic!(
                "expected InvalidBatchOperation error, got different error: {:?}",
                other
            );
        }
        Ok(()) => {
            panic!("expected InvalidBatchOperation error, but apply_batch succeeded");
        }
    }
}

#[test]
fn test_apply_batch_rejects_insert_non_merk_tree() {
    let db = make_empty_grovedb();
    let grove_version = GroveVersion::latest();

    let op = QualifiedGroveDbOp {
        path: KeyInfoPath(vec![]),
        key: Some(KnownKey(b"test".to_vec())),
        op: GroveOp::InsertNonMerkTree {
            hash: [0u8; 32],
            root_key: None,
            flags: None,
            aggregate_data: AggregateData::NoAggregateData,
            meta: NonMerkTreeMeta::MmrTree { mmr_size: 0 },

            non_counted: false,
        },
    };

    let result = db.apply_batch(vec![op], None, None, grove_version).value;

    match result {
        Err(Error::InvalidBatchOperation(_)) => {
            // Correctly rejected -- the consistency check fires first with
            // "batch operations fail consistency checks", blocking the
            // internal-only InsertNonMerkTree op.
        }
        Err(other) => {
            panic!(
                "expected InvalidBatchOperation error, got different error: {:?}",
                other
            );
        }
        Ok(()) => {
            panic!("expected InvalidBatchOperation error, but apply_batch succeeded");
        }
    }
}

// ===========================================================================
// ReplaceNonMerkTreeRoot meta-type mismatch test
// ===========================================================================

/// Test that `ReplaceNonMerkTreeRoot` with a meta type that does not match
/// the existing element type is rejected by the batch consistency check.
///
/// We insert an MmrTree, then submit a batch with `ReplaceNonMerkTreeRoot`
/// carrying `DenseTree` meta targeting the same key. Since
/// `ReplaceNonMerkTreeRoot` is an internal-only op, the consistency check
/// should reject it before we even reach the meta-type mismatch.
#[test]
fn test_apply_batch_replace_non_merk_tree_root_wrong_meta() {
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
    .expect("insert parent tree");

    // Insert an MmrTree as child
    db.insert(
        [b"parent"].as_ref(),
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    // Append a value so the tree has state
    db.mmr_tree_append(
        [b"parent"].as_ref(),
        b"mmr",
        b"data".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("append to mmr");

    // Construct a ReplaceNonMerkTreeRoot with WRONG meta type (DenseTree instead
    // of MmrTree). This op is internal-only, so the consistency check should
    // reject it before the meta mismatch matters.
    let op = QualifiedGroveDbOp {
        path: KeyInfoPath(vec![KnownKey(b"parent".to_vec())]),
        key: Some(KnownKey(b"mmr".to_vec())),
        op: GroveOp::ReplaceNonMerkTreeRoot {
            hash: [0u8; 32],
            meta: NonMerkTreeMeta::DenseTree {
                count: 1,
                height: 3,
            },
        },
    };

    let result = db.apply_batch(vec![op], None, None, grove_version).value;

    // ReplaceNonMerkTreeRoot is an internal-only op — the consistency check
    // rejects it with InvalidBatchOperation, regardless of meta type.
    match result {
        Err(Error::InvalidBatchOperation(_)) => {
            // Correctly rejected — the consistency check catches internal-only
            // ops before they can be processed. The meta-type
            // mismatch would be a secondary concern but never gets
            // reached.
        }
        Err(other) => {
            panic!(
                "expected InvalidBatchOperation error for ReplaceNonMerkTreeRoot with mismatched \
                 meta, got different error: {:?}",
                other
            );
        }
        Ok(()) => {
            panic!(
                "expected InvalidBatchOperation error, but apply_batch succeeded — this means \
                 ReplaceNonMerkTreeRoot with mismatched meta was silently accepted"
            );
        }
    }
}

// ===========================================================================
// Cross-tree-type mixed batch test
// ===========================================================================

/// Helper: generate a deterministic 32-byte cmx from an index (same as
/// commitment_tree_tests).
fn test_cmx(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[31] &= 0x7f;
    bytes
}

/// Helper: create a deterministic 32-byte rho (nullifier) from an index.
fn test_rho(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[1] = 0xAA;
    bytes
}

/// Helper: create a deterministic 32-byte cv_net (value commitment) from an
/// index.
fn test_cv_net(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[1] = 0xCC;
    bytes
}

/// Helper: create a deterministic test ciphertext for DashMemo.
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

/// Test that a single batch can operate on all four non-Merk tree types
/// simultaneously: CommitmentTree, MmrTree, BulkAppendTree, and DenseTree.
///
/// Inserts all four tree types individually first, then applies a single batch
/// with one op for each tree type. Verifies all four trees updated correctly.
#[test]
fn test_batch_all_four_non_merk_tree_types() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert parent tree to hold all four non-Merk trees
    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");

    // Insert each non-Merk tree type under parent
    db.insert(
        [b"parent"].as_ref(),
        b"ct",
        Element::empty_commitment_tree(10).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    db.insert(
        [b"parent"].as_ref(),
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    db.insert(
        [b"parent"].as_ref(),
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

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

    // Record the GroveDB root hash before the mixed batch
    let root_hash_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash before batch");

    // Build a single batch with one op for each non-Merk tree type
    let ops = vec![
        QualifiedGroveDbOp::commitment_tree_insert_op_typed(
            vec![b"parent".to_vec(), b"ct".to_vec()],
            test_cmx(1),
            test_rho(1),
            test_cv_net(1),
            &test_ciphertext(1),
        ),
        QualifiedGroveDbOp::mmr_tree_append_op(
            vec![b"parent".to_vec(), b"mmr".to_vec()],
            b"mmr_value".to_vec(),
        ),
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec(), b"bulk".to_vec()],
            b"bulk_value".to_vec(),
        ),
        QualifiedGroveDbOp::dense_tree_insert_op(
            vec![b"parent".to_vec(), b"dense".to_vec()],
            b"dense_value".to_vec(),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("mixed batch with all four non-Merk tree types");

    // Verify CommitmentTree: count should be 1
    let ct_count = db
        .commitment_tree_count([b"parent"].as_ref(), b"ct", None, grove_version)
        .unwrap()
        .expect("commitment tree count after batch");
    assert_eq!(ct_count, 1, "commitment tree should have 1 note");

    // Verify MmrTree: leaf count should be 1
    let mmr_count = db
        .mmr_tree_leaf_count([b"parent"].as_ref(), b"mmr", None, grove_version)
        .unwrap()
        .expect("mmr tree leaf count after batch");
    assert_eq!(mmr_count, 1, "mmr tree should have 1 leaf");

    // Verify BulkAppendTree: count should be 1
    let bulk_count = db
        .bulk_count([b"parent"].as_ref(), b"bulk", None, grove_version)
        .unwrap()
        .expect("bulk append tree count after batch");
    assert_eq!(bulk_count, 1, "bulk append tree should have 1 entry");

    // Verify DenseTree: count should be 1
    let dense_count = db
        .dense_tree_count([b"parent"].as_ref(), b"dense", None, grove_version)
        .unwrap()
        .expect("dense tree count after batch");
    assert_eq!(dense_count, 1, "dense tree should have 1 entry");

    // Verify the dense tree value is correct
    let dense_val = db
        .dense_tree_get([b"parent"].as_ref(), b"dense", 0, None, grove_version)
        .unwrap()
        .expect("get dense tree value");
    assert_eq!(
        dense_val,
        Some(b"dense_value".to_vec()),
        "dense tree value should match"
    );

    // Verify the MMR root is non-zero (not empty)
    let mmr_root = db
        .mmr_tree_root_hash([b"parent"].as_ref(), b"mmr", None, grove_version)
        .unwrap()
        .expect("mmr root hash");
    assert_ne!(mmr_root, [0u8; 32], "mmr root should not be zero");

    // Verify the GroveDB root hash changed
    let root_hash_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash after batch");
    assert_ne!(
        root_hash_before, root_hash_after,
        "GroveDB root hash should change after mixed batch"
    );

    // Verify the database is consistent
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb should not fail");
    assert!(
        issues.is_empty(),
        "expected no issues after mixed batch, got: {:?}",
        issues
    );
}

// ===========================================================================
// Debug formatting coverage for CommitmentTreeInsert
// ===========================================================================

/// Test that the Debug implementation for QualifiedGroveDbOp correctly
/// formats a CommitmentTreeInsert op, including hex-encoded cmx and rho
/// prefixes.
#[test]
fn test_commitment_tree_insert_debug_format() {
    let mut cmx = [0u8; 32];
    cmx[0] = 0xAB;
    cmx[1] = 0xCD;
    cmx[2] = 0xEF;
    cmx[3] = 0x01;

    let mut rho = [0u8; 32];
    rho[0] = 0x12;
    rho[1] = 0x34;
    rho[2] = 0x56;
    rho[3] = 0x78;

    let cv_net = [0x9Au8; 32];

    let op = QualifiedGroveDbOp::commitment_tree_insert_op(
        vec![b"pool".to_vec()],
        cmx,
        rho,
        cv_net,
        b"payload".to_vec(),
    );

    let debug_str = format!("{:?}", op);

    assert!(
        debug_str.contains("abcdef01"),
        "debug output should contain hex-encoded cmx prefix, got: {}",
        debug_str
    );
    assert!(
        debug_str.contains("12345678"),
        "debug output should contain hex-encoded rho prefix, got: {}",
        debug_str
    );
    assert!(
        debug_str.contains("Commitment Tree Insert"),
        "debug output should contain op name, got: {}",
        debug_str
    );
}

// ===========================================================================
// Keyed child writes under non-Merk (append-family) parents — issue #900
// ===========================================================================
//
// Non-Merk data trees (CommitmentTree, MmrTree, BulkAppendTree, DenseTree,
// PrivateDocumentStore) keep their state in the data namespace and commit a
// typed root. Before the V4 gate `non_merk_parent_keyed_ops_rejection`,
// ordinary keyed batch ops beneath such a parent executed through Merk
// dispatch against the parent's (empty) Merk namespace: the batch returned
// Ok, the level's Merk root was committed into the parent element's hash
// while the element kept its typed metadata (e.g. `mmr_size: 0`) and no
// root key — so the acknowledged rows were unreachable, typed readers saw
// an empty tree, and verify_grovedb reported the subtree corrupted. These
// tests pin the V4+ rejection for parents created in the same batch and
// for pre-existing parents, and that valid typed usage is unaffected.

/// The four append-family elements (PrivateDocumentStore has its own
/// dedicated rejection tests in private_document_store_tests.rs).
fn append_family_elements() -> Vec<(&'static [u8], Element)> {
    vec![
        (
            b"ct".as_slice(),
            Element::empty_commitment_tree(10).expect("valid chunk_power"),
        ),
        (b"mmr".as_slice(), Element::empty_mmr_tree()),
        (
            b"bulk".as_slice(),
            Element::empty_bulk_append_tree(2).expect("valid chunk_power"),
        ),
        (b"dense".as_slice(), Element::empty_dense_tree(3)),
    ]
}

#[test]
fn test_batch_rejects_keyed_child_under_same_batch_created_append_tree() {
    let grove_version = GroveVersion::latest();
    for (key, element) in append_family_elements() {
        let db = make_empty_grovedb();
        let root_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash before");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(vec![], key.to_vec(), element),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![key.to_vec()],
                b"child".to_vec(),
                Element::new_item(b"data".to_vec()),
            ),
        ];
        let result = db.apply_batch(ops, None, None, grove_version).value;
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "keyed child under same-batch {} parent must be rejected, got {:?}",
            String::from_utf8_lossy(key),
            result
        );

        // The whole batch must roll back: no parent element, unchanged root.
        assert!(
            db.get(EMPTY_PATH, key, None, grove_version)
                .unwrap()
                .is_err(),
            "rejected batch must not have created the {} parent",
            String::from_utf8_lossy(key)
        );
        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash after");
        assert_eq!(root_before, root_after, "root hash must be unchanged");
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb should not fail");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }
}

#[test]
fn test_batch_rejects_keyed_child_under_existing_append_tree() {
    let grove_version = GroveVersion::latest();
    for (key, element) in append_family_elements() {
        let db = make_empty_grovedb();
        db.insert(EMPTY_PATH, key, element.clone(), None, None, grove_version)
            .unwrap()
            .expect("insert append-family tree");
        let root_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash before");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![key.to_vec()],
            b"child".to_vec(),
            Element::new_item(b"data".to_vec()),
        )];
        let result = db.apply_batch(ops, None, None, grove_version).value;
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "keyed child under existing {} parent must be rejected, got {:?}",
            String::from_utf8_lossy(key),
            result
        );

        // State must be untouched: same root, same stored element, clean
        // verification.
        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash after");
        assert_eq!(root_before, root_after, "root hash must be unchanged");
        let stored = db
            .get(EMPTY_PATH, key, None, grove_version)
            .unwrap()
            .expect("parent element still readable");
        assert_eq!(stored, element, "parent element must be unchanged");
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb should not fail");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }
}

#[test]
fn test_batch_rejects_keyed_delete_under_existing_append_tree() {
    // Not just inserts: EVERY keyed op family dispatching into the non-Merk
    // tree's own path is refused.
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

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![b"mmr".to_vec()],
        b"child".to_vec(),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).value;
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "keyed delete under an MmrTree parent must be rejected, got {:?}",
        result
    );
}

#[test]
fn test_batch_rejects_subtree_created_below_append_tree() {
    // A grandchild level executes before its parents (deepest level first),
    // so the ordinary Tree level applies cleanly — the rejection fires when
    // its root propagates into the MmrTree's own path, and the whole batch
    // must roll back, including the already-executed grandchild level.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let root_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash before");

    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"mmr".to_vec(),
            Element::empty_mmr_tree(),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"mmr".to_vec()],
            b"sub".to_vec(),
            Element::empty_tree(),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"mmr".to_vec(), b"sub".to_vec()],
            b"child".to_vec(),
            Element::new_item(b"data".to_vec()),
        ),
    ];
    let result = db.apply_batch(ops, None, None, grove_version).value;
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "subtree nested below an MmrTree parent must be rejected, got {:?}",
        result
    );
    let root_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash after");
    assert_eq!(root_before, root_after, "root hash must be unchanged");
    assert!(
        db.get(EMPTY_PATH, b"mmr", None, grove_version)
            .unwrap()
            .is_err(),
        "rejected batch must not have created the mmr parent"
    );
}

#[test]
fn test_batch_creating_empty_append_trees_still_succeeds() {
    // Valid fresh-parent creation (no keyed descendants) is unaffected by
    // the rejection, and the created trees agree with typed readers and
    // integrity checks.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let ops = append_family_elements()
        .into_iter()
        .map(|(key, element)| {
            QualifiedGroveDbOp::insert_or_replace_op(vec![], key.to_vec(), element)
        })
        .collect::<Vec<_>>();
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("creating empty append-family trees in a batch must succeed");

    // Typed appends into the freshly created trees work.
    db.mmr_tree_append(EMPTY_PATH, b"mmr", b"value".to_vec(), None, grove_version)
        .unwrap()
        .expect("typed append into fresh mmr tree");
    assert_eq!(
        db.mmr_tree_leaf_count(EMPTY_PATH, b"mmr", None, grove_version)
            .unwrap()
            .expect("leaf count"),
        1
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb should not fail");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

#[test]
fn test_batch_keyed_child_under_append_tree_accepted_on_grove_v3_for_replay() {
    // GROVE_V3 must keep the released accepted/rejected outcome: the gate
    // `non_merk_parent_keyed_ops_rejection` is 0, so the (corrupting) keyed
    // child write under a CommitmentTree parent still returns Ok. This
    // documents that the V4 fix does not alter V1..V3 replay.
    use grovedb_version::version::v3::GROVE_V3;
    let db = make_empty_grovedb();
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"ct".to_vec(),
            Element::empty_commitment_tree(10).expect("valid chunk_power"),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"ct".to_vec()],
            b"child".to_vec(),
            Element::new_item(b"data".to_vec()),
        ),
    ];
    db.apply_batch(ops, None, None, &GROVE_V3)
        .unwrap()
        .expect("GROVE_V3 must preserve the released (accepting) behavior");
}
