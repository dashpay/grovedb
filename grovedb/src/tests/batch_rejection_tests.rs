//! Tests for batch operation rejection of internal-only ops.
//!
//! Internal-only operations (`ReplaceTreeRootKey`, `InsertTreeWithRootHash`,
//! `InsertNonMerkTree`) are produced exclusively by propagation or
//! preprocessing. They must never be accepted when submitted directly via
//! `apply_batch`.
//!
//! Two layers enforce this:
//! 1. `verify_consistency_of_operations` (consistency check) catches them
//!    first with "batch operations fail consistency checks".
//! 2. `from_ops` in `batch_structure.rs` provides defense-in-depth with
//!    "replace and insert tree hash are internal operations only".
//!
//! These tests verify layer 1 (the consistency check), which is the first
//! guard that fires at the `apply_batch` entry point.

use grovedb_merk::tree::AggregateData;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{
        key_info::KeyInfo::KnownKey, GroveOp, KeyInfoPath, NonMerkTreeMeta, QualifiedGroveDbOp,
    },
    tests::make_empty_grovedb,
    Error,
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

    let result = db
        .apply_batch(vec![op], None, None, grove_version)
        .value;

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
        },
    };

    let result = db
        .apply_batch(vec![op], None, None, grove_version)
        .value;

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
        },
    };

    let result = db
        .apply_batch(vec![op], None, None, grove_version)
        .value;

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
