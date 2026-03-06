//! Targeted coverage tests for lib.rs, batch cost estimators, merk_cache, and
//! proof util.
//!
//! This module fills coverage gaps in:
//! - `lib.rs` (open, root_hash, verify_grovedb, transactions, flush)
//! - `batch/estimated_costs/average_case_costs.rs` (batch-level average cost
//!   estimation)
//! - `batch/estimated_costs/worst_case_costs.rs` (batch-level worst case cost
//!   estimation)
//! - `merk_cache.rs` (indirect coverage through batch operations)
//! - `operations/proof/util.rs` (hex_to_ascii, path helpers, Display impls,
//!   conversions)

use std::collections::HashMap;

use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
use grovedb_merk::{
    estimated_costs::{
        average_case_costs::{
            EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes, EstimatedSumTrees,
        },
        worst_case_costs::WorstCaseLayerInformation,
    },
    proofs::query::ProvedKeyValue,
    tree_type::TreeType,
};
use grovedb_version::version::GroveVersion;
use tempfile::TempDir;

use super::*;
use crate::{
    batch::{
        estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
        key_info::KeyInfo,
        KeyInfoPath, QualifiedGroveDbOp,
    },
    operations::proof::util::{
        element_hex_to_ascii, hex_to_ascii, optional_element_hex_to_ascii,
        path_as_slices_hex_to_ascii, path_hex_to_ascii, ProvedPathKeyOptionalValue,
        ProvedPathKeyValue,
    },
    tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb},
    Element, GroveDb,
};

// ===========================================================================
// lib.rs coverage: open, root_hash, verify_grovedb, transactions, flush
// ===========================================================================

#[test]
fn open_db_at_path() {
    let tmp_dir = TempDir::new().expect("should create temp dir");
    let db = GroveDb::open(tmp_dir.path()).expect("should open a new GroveDB");
    // Verify the DB is usable by getting the root hash
    let grove_version = GroveVersion::latest();
    let _root_hash = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash from freshly opened DB");
}

#[test]
fn root_hash_empty_db() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let root_hash_1 = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash of empty DB");
    let root_hash_2 = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash again");
    assert_eq!(
        root_hash_1, root_hash_2,
        "root hash of empty DB should be deterministic"
    );
}

#[test]
fn root_hash_with_data() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert an item into one of the test leaves
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key1",
        Element::new_item(b"value1".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    let root_hash = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash after insert");
    assert_ne!(
        root_hash, [0u8; 32],
        "root hash with data should not be all zeros"
    );
}

#[test]
fn root_hash_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    // Insert within the transaction
    db.insert(
        [TEST_LEAF].as_ref(),
        b"tx_key",
        Element::new_item(b"tx_value".to_vec()),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("should insert within transaction");

    // Root hash within the transaction should reflect the insert
    let root_hash_in_tx = db
        .root_hash(Some(&tx), grove_version)
        .unwrap()
        .expect("should get root hash within transaction");

    // Root hash outside the transaction should not reflect it yet
    let root_hash_outside = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash outside transaction");

    assert_ne!(
        root_hash_in_tx, root_hash_outside,
        "root hash inside vs outside transaction should differ"
    );
}

#[test]
fn root_key_empty_db() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let root_key = db
        .root_key(None, grove_version)
        .unwrap()
        .expect("should get root key of empty DB");
    assert!(root_key.is_none(), "root key of empty DB should be None");
}

#[test]
fn root_key_with_data() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let root_key = db
        .root_key(None, grove_version)
        .unwrap()
        .expect("should get root key after inserts");
    assert!(
        root_key.is_some(),
        "root key should be Some after inserting trees"
    );
}

#[test]
fn verify_grovedb_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let issues = db
        .verify_grovedb(None, true, true, grove_version)
        .expect("verify_grovedb on empty DB should succeed");
    assert!(
        issues.is_empty(),
        "empty DB should have no verification issues"
    );
}

#[test]
fn verify_grovedb_with_data() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert various element types
    db.insert(
        [TEST_LEAF].as_ref(),
        b"item_key",
        Element::new_item(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.insert(
        [TEST_LEAF].as_ref(),
        b"subtree_key",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert subtree");

    db.insert(
        [TEST_LEAF, b"subtree_key"].as_ref(),
        b"nested_item",
        Element::new_item(b"nested_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert nested item");

    let issues = db
        .verify_grovedb(None, false, true, grove_version)
        .expect("verify_grovedb with data should succeed");
    assert!(
        issues.is_empty(),
        "verify_grovedb should report no issues on consistent data, got: {:?}",
        issues
    );
}

#[test]
fn verify_grovedb_with_references() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert an item and a reference to it
    db.insert(
        [TEST_LEAF].as_ref(),
        b"target",
        Element::new_item(b"target_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert target item");

    db.insert(
        [ANOTHER_TEST_LEAF].as_ref(),
        b"ref_key",
        Element::new_reference(
            crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target".to_vec(),
            ]),
        ),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert reference");

    let issues = db
        .verify_grovedb(None, true, true, grove_version)
        .expect("verify_grovedb with references should succeed");
    assert!(
        issues.is_empty(),
        "verify_grovedb with valid references should have no issues, got: {:?}",
        issues
    );
}

#[test]
fn verify_grovedb_with_sum_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert a sum tree and sum items
    db.insert(
        [TEST_LEAF].as_ref(),
        b"sum_tree",
        Element::empty_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert sum tree");

    db.insert(
        [TEST_LEAF, b"sum_tree"].as_ref(),
        b"s1",
        Element::new_sum_item(42),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert sum item");

    db.insert(
        [TEST_LEAF, b"sum_tree"].as_ref(),
        b"s2",
        Element::new_sum_item(100),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert second sum item");

    let issues = db
        .verify_grovedb(None, false, true, grove_version)
        .expect("verify_grovedb with sum tree should succeed");
    assert!(
        issues.is_empty(),
        "sum tree should pass verification, got: {:?}",
        issues
    );
}

#[test]
fn visualize_verify_grovedb_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let result = db
        .visualize_verify_grovedb(None, false, true, grove_version)
        .expect("visualize_verify_grovedb should succeed on empty DB");
    assert!(
        result.is_empty(),
        "visualization of empty DB verification should be empty"
    );
}

#[test]
fn visualize_verify_grovedb_with_data() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"key",
        Element::new_item(b"val".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    let result = db
        .visualize_verify_grovedb(None, false, true, grove_version)
        .expect("visualize_verify_grovedb should succeed");
    assert!(
        result.is_empty(),
        "consistent data should produce empty visualization"
    );
}

#[test]
fn grove_db_reopen() {
    let grove_version = GroveVersion::latest();
    let tmp_dir = TempDir::new().expect("should create temp dir");
    let path = tmp_dir.path().to_path_buf();

    // Open, insert data, then drop to close
    {
        let db = GroveDb::open(&path).expect("should open DB");
        db.insert(
            EMPTY_PATH,
            b"tree1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"tree1"].as_ref(),
            b"item1",
            Element::new_item(b"persisted".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
    }

    // Reopen and verify data persists
    let db = GroveDb::open(&path).expect("should reopen DB");
    let element = db
        .get([b"tree1"].as_ref(), b"item1", None, grove_version)
        .unwrap()
        .expect("should get persisted item after reopen");
    assert_eq!(
        element,
        Element::new_item(b"persisted".to_vec()),
        "data should persist after close and reopen"
    );
}

#[test]
fn transaction_commit() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    db.insert(
        [TEST_LEAF].as_ref(),
        b"tx_item",
        Element::new_item(b"committed".to_vec()),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("should insert in transaction");

    // Not visible outside transaction
    let result = db
        .get([TEST_LEAF].as_ref(), b"tx_item", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "item should not be visible before commit");

    // Commit
    db.commit_transaction(tx)
        .unwrap()
        .expect("should commit transaction");

    // Now visible
    let element = db
        .get([TEST_LEAF].as_ref(), b"tx_item", None, grove_version)
        .unwrap()
        .expect("should get item after commit");
    assert_eq!(element, Element::new_item(b"committed".to_vec()));
}

#[test]
fn transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    db.insert(
        [TEST_LEAF].as_ref(),
        b"rollback_item",
        Element::new_item(b"should_disappear".to_vec()),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("should insert in transaction");

    // Rollback
    db.rollback_transaction(&tx)
        .expect("should rollback transaction");

    // Should not be visible even within the transaction after rollback
    let result = db
        .get([TEST_LEAF].as_ref(), b"rollback_item", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "item should not exist after rollback");
}

#[test]
fn flush_db() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"flush_key",
        Element::new_item(b"flush_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.flush().expect("flush should succeed");

    // Data should still be accessible after flush
    let element = db
        .get([TEST_LEAF].as_ref(), b"flush_key", None, grove_version)
        .unwrap()
        .expect("should get item after flush");
    assert_eq!(element, Element::new_item(b"flush_value".to_vec()));
}

#[test]
fn wipe_db() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"wipe_key",
        Element::new_item(b"wipe_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    db.wipe().expect("wipe should succeed");
}

// ===========================================================================
// batch/estimated_costs — average case: insert items, delete, mixed
// ===========================================================================

/// Helper for average case with items sizing.
fn avg_items_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
        estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
    }
}

#[test]
fn batch_average_case_insert_item_cost() {
    let grove_version = GroveVersion::latest();
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"item_key".to_vec(),
        Element::new_item(b"some_value".to_vec()),
    )];
    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), avg_items_layer_info());

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case item insert cost should succeed");

    assert!(
        cost.seek_count > 0,
        "average case insert should have non-zero seek count"
    );
    assert!(
        cost.storage_cost.added_bytes > 0,
        "average case insert should add bytes"
    );
    assert!(
        cost.hash_node_calls > 0,
        "average case insert should have hash calls"
    );
}

#[test]
fn batch_average_case_delete_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![],
        b"item_to_delete".to_vec(),
    )];

    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), avg_items_layer_info());

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case delete cost should succeed");

    assert!(
        cost.seek_count > 0,
        "average case delete should have non-zero seek count"
    );
}

#[test]
fn batch_average_case_mixed_operations_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"new_tree".to_vec(),
            Element::empty_tree(),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"item_key".to_vec(),
            Element::new_item(b"value".to_vec()),
        ),
    ];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    // We need path info for the new tree subtree since we're inserting a tree
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"new_tree".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case mixed ops cost should succeed");

    assert!(
        cost.seek_count > 0,
        "mixed ops should have non-zero seek count"
    );
    assert!(cost.hash_node_calls > 0, "mixed ops should have hash calls");
}

#[test]
fn batch_average_case_replace_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![],
        b"existing_key".to_vec(),
        Element::new_item(b"new_value".to_vec()),
    )];

    let mut paths = HashMap::new();
    paths.insert(KeyInfoPath(vec![]), avg_items_layer_info());

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case replace cost should succeed");

    assert!(cost.seek_count > 0, "replace should have seeks");
}

// ===========================================================================
// batch/estimated_costs — worst case: insert items, delete, comparison
// ===========================================================================

#[test]
fn batch_worst_case_insert_item_cost() {
    let grove_version = GroveVersion::latest();
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"wc_item".to_vec(),
        Element::new_item(b"worst_case_value".to_vec()),
    )];
    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(100),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case item insert cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case insert should have non-zero seek count"
    );
    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case insert should add bytes"
    );
}

#[test]
fn batch_worst_case_delete_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![b"leaf".to_vec()],
        b"to_delete".to_vec(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(100),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(100),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case delete cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case delete should have non-zero seek count"
    );
}

#[test]
fn batch_worst_case_gte_average_case() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"compare_key".to_vec(),
        Element::new_item(b"compare_value".to_vec()),
    )];

    // Average case
    let mut avg_paths = HashMap::new();
    avg_paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        },
    );
    let avg_cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(avg_paths),
        ops.clone(),
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case cost should succeed");

    // Worst case
    let mut wc_paths = HashMap::new();
    wc_paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(100),
    );
    let wc_cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(wc_paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case cost should succeed");

    assert!(
        wc_cost.worse_or_eq_than(&avg_cost),
        "worst case cost {:?} should be >= average case cost {:?}",
        wc_cost,
        avg_cost
    );
}

#[test]
fn batch_worst_case_replace_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"existing".to_vec(),
        Element::new_item(b"replaced".to_vec()),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case replace cost should succeed");

    assert!(cost.seek_count > 0, "worst case replace should have seeks");
}

#[test]
fn batch_average_case_insert_tree_cost_actual_comparison() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let tx = db.start_transaction();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"newtree".to_vec(),
        Element::empty_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                7,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );

    let estimated_cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops.clone(),
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case tree insert cost should succeed");

    let actual_cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;

    // Average case added_bytes should match actual for a known insert
    assert_eq!(
        estimated_cost.storage_cost.added_bytes, actual_cost.storage_cost.added_bytes,
        "estimated added bytes should match actual for tree insert"
    );
}

// ===========================================================================
// merk_cache.rs — indirect coverage through batch operations that use it
// ===========================================================================

#[test]
fn batch_operations_exercise_merk_cache() {
    // MerkCache is used internally by batch processing. By running a
    // multi-path batch operation, we exercise the cache's get_merk, into_batch,
    // and propagation logic.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    // Insert a subtree under TEST_LEAF
    db.insert(
        [TEST_LEAF].as_ref(),
        b"subtree_a",
        Element::empty_tree(),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("should insert subtree_a");

    // Now batch insert multiple items across different paths
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"subtree_a".to_vec()],
            b"k1".to_vec(),
            Element::new_item(b"v1".to_vec()),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"subtree_a".to_vec()],
            b"k2".to_vec(),
            Element::new_item(b"v2".to_vec()),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![ANOTHER_TEST_LEAF.to_vec()],
            b"k3".to_vec(),
            Element::new_item(b"v3".to_vec()),
        ),
    ];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch across multiple paths should succeed");

    // Verify items were inserted
    let elem = db
        .get(
            [TEST_LEAF, b"subtree_a"].as_ref(),
            b"k1",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should get k1");
    assert_eq!(elem, Element::new_item(b"v1".to_vec()));

    let elem = db
        .get(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"k3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should get k3");
    assert_eq!(elem, Element::new_item(b"v3".to_vec()));

    // Verify grovedb is consistent after batch
    db.commit_transaction(tx).unwrap().expect("should commit");

    let issues = db
        .verify_grovedb(None, false, true, grove_version)
        .expect("verify should succeed after batch");
    assert!(
        issues.is_empty(),
        "no verification issues after batch: {:?}",
        issues
    );
}

// ===========================================================================
// operations/proof/util.rs — hex_to_ascii, path helpers, Display, conversions
// ===========================================================================

#[test]
fn hex_to_ascii_with_ascii_bytes() {
    // All ASCII allowed characters should produce a readable string
    let ascii_input = b"Hello_World-123";
    let result = hex_to_ascii(ascii_input);
    assert_eq!(
        result, "Hello_World-123",
        "ASCII-only input should produce readable string"
    );
}

#[test]
fn hex_to_ascii_with_binary_bytes() {
    // Non-ASCII bytes should produce hex-encoded string
    let binary_input = vec![0xFF, 0x00, 0xAB];
    let result = hex_to_ascii(&binary_input);
    assert!(
        result.starts_with("0x"),
        "binary input should produce hex-prefixed string"
    );
    assert_eq!(result, "0xff00ab", "hex encoding should be lowercase");
}

#[test]
fn hex_to_ascii_empty_input() {
    let result = hex_to_ascii(b"");
    assert_eq!(result, "", "empty input should produce empty string");
}

#[test]
fn hex_to_ascii_mixed_allowed_and_disallowed() {
    // Contains a space which is not in ALLOWED_CHARS
    let input = b"Hello World";
    let result = hex_to_ascii(input);
    assert!(
        result.starts_with("0x"),
        "input with disallowed chars should produce hex-encoded string"
    );
}

#[test]
fn hex_to_ascii_special_allowed_chars() {
    // Test @, /, \, [, ] which are in the allowed set
    let input = b"path/to/key@[0]";
    let result = hex_to_ascii(input);
    assert_eq!(
        result, "path/to/key@[0]",
        "special allowed characters should produce readable string"
    );
}

#[test]
fn path_hex_to_ascii_simple() {
    let path: Vec<Vec<u8>> = vec![b"tree1".to_vec(), b"subtree".to_vec(), b"key".to_vec()];
    let result = path_hex_to_ascii(&path);
    assert_eq!(
        result, "tree1/subtree/key",
        "ASCII path segments should produce readable joined path"
    );
}

#[test]
fn path_hex_to_ascii_with_binary() {
    let path: Vec<Vec<u8>> = vec![b"tree1".to_vec(), vec![0xFF, 0xAB]];
    let result = path_hex_to_ascii(&path);
    assert_eq!(
        result, "tree1/0xffab",
        "mixed path should have readable and hex segments"
    );
}

#[test]
fn path_hex_to_ascii_empty() {
    let path: Vec<Vec<u8>> = vec![];
    let result = path_hex_to_ascii(&path);
    assert_eq!(result, "", "empty path should produce empty string");
}

#[test]
fn path_as_slices_hex_to_ascii_simple() {
    let path: &[&[u8]] = &[b"a", b"b", b"c"];
    let result = path_as_slices_hex_to_ascii(path);
    assert_eq!(
        result, "a/b/c",
        "ASCII slices should produce readable joined path"
    );
}

#[test]
fn path_as_slices_hex_to_ascii_with_binary() {
    let path: &[&[u8]] = &[b"tree", &[0x01, 0x02]];
    let result = path_as_slices_hex_to_ascii(path);
    assert_eq!(
        result, "tree/0x0102",
        "mixed slices should have readable and hex segments"
    );
}

#[test]
fn proved_path_key_value_from_proved_key_value() {
    let path = vec![b"path1".to_vec(), b"path2".to_vec()];
    let pkv = ProvedKeyValue {
        key: b"mykey".to_vec(),
        value: vec![1, 2, 3],
        proof: [42u8; 32],
        value_hash_is_computed: true,
        is_reference_result: false,
    };
    let result = ProvedPathKeyValue::from_proved_key_value(path.clone(), pkv);
    assert_eq!(result.path, path);
    assert_eq!(result.key, b"mykey".to_vec());
    assert_eq!(result.value, vec![1, 2, 3]);
    assert_eq!(result.proof, [42u8; 32]);
}

#[test]
fn proved_path_key_values_from_multiple() {
    let path = vec![b"p".to_vec()];
    let pkvs = vec![
        ProvedKeyValue {
            key: b"a".to_vec(),
            value: vec![10],
            proof: [0; 32],
            value_hash_is_computed: true,
            is_reference_result: false,
        },
        ProvedKeyValue {
            key: b"b".to_vec(),
            value: vec![20],
            proof: [1; 32],
            value_hash_is_computed: true,
            is_reference_result: false,
        },
    ];
    let results = ProvedPathKeyValue::from_proved_key_values(path.clone(), pkvs);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].key, b"a".to_vec());
    assert_eq!(results[1].key, b"b".to_vec());
    assert_eq!(results[0].path, path);
    assert_eq!(results[1].path, path);
}

#[test]
fn proved_path_key_optional_value_try_from_with_value() {
    let optional = ProvedPathKeyOptionalValue {
        path: vec![b"p".to_vec()],
        key: b"k".to_vec(),
        value: Some(vec![1, 2, 3]),
        proof: [5; 32],
    };
    let result: ProvedPathKeyValue = optional
        .try_into()
        .expect("should convert optional with value to ProvedPathKeyValue");
    assert_eq!(result.key, b"k".to_vec());
    assert_eq!(result.value, vec![1, 2, 3]);
}

#[test]
fn proved_path_key_optional_value_try_from_none_fails() {
    let optional = ProvedPathKeyOptionalValue {
        path: vec![b"p".to_vec()],
        key: b"missing".to_vec(),
        value: None,
        proof: [0; 32],
    };
    let result: Result<ProvedPathKeyValue, _> = optional.try_into();
    assert!(
        result.is_err(),
        "converting optional with None value should fail"
    );
}

#[test]
fn proved_path_key_value_into_optional() {
    let pkv = ProvedPathKeyValue {
        path: vec![b"p".to_vec()],
        key: b"k".to_vec(),
        value: vec![1, 2, 3],
        proof: [9; 32],
    };
    let optional: ProvedPathKeyOptionalValue = pkv.into();
    assert_eq!(optional.value, Some(vec![1, 2, 3]));
    assert_eq!(optional.key, b"k".to_vec());
}

#[test]
fn proved_path_key_value_display() {
    let grove_version = GroveVersion::latest();
    let item = Element::new_item(b"hello".to_vec());
    let serialized = item
        .serialize(grove_version)
        .expect("should serialize item");

    let pkv = ProvedPathKeyValue {
        path: vec![b"tree1".to_vec()],
        key: b"mykey".to_vec(),
        value: serialized,
        proof: [0; 32],
    };

    let display = format!("{}", pkv);
    assert!(
        display.contains("ProvedPathKeyValue"),
        "Display should contain type name"
    );
    assert!(
        display.contains("tree1"),
        "Display should contain readable path"
    );
    assert!(
        display.contains("mykey"),
        "Display should contain readable key"
    );
}

#[test]
fn proved_path_key_optional_value_display() {
    let grove_version = GroveVersion::latest();
    let item = Element::new_item(b"world".to_vec());
    let serialized = item
        .serialize(grove_version)
        .expect("should serialize item");

    let pkv = ProvedPathKeyOptionalValue {
        path: vec![b"tree2".to_vec()],
        key: b"optkey".to_vec(),
        value: Some(serialized),
        proof: [1; 32],
    };

    let display = format!("{}", pkv);
    assert!(
        display.contains("ProvedPathKeyValue"),
        "Display should contain type name"
    );
    assert!(
        display.contains("tree2"),
        "Display should contain readable path"
    );
}

#[test]
fn proved_path_key_optional_value_display_none() {
    let pkv = ProvedPathKeyOptionalValue {
        path: vec![b"tree3".to_vec()],
        key: b"nonekey".to_vec(),
        value: None,
        proof: [2; 32],
    };

    let display = format!("{}", pkv);
    assert!(
        display.contains("None"),
        "Display with None value should contain 'None'"
    );
}

#[test]
fn element_hex_to_ascii_with_valid_element() {
    let grove_version = GroveVersion::latest();
    let item = Element::new_item(b"test_data".to_vec());
    let serialized = item
        .serialize(grove_version)
        .expect("should serialize item");

    let result =
        element_hex_to_ascii(&serialized).expect("should deserialize and display valid element");
    assert!(
        !result.is_empty(),
        "display of valid element should not be empty"
    );
}

#[test]
fn element_hex_to_ascii_with_invalid_bytes() {
    let invalid = vec![0xFF, 0xFE, 0xFD, 0xFC];
    let result = element_hex_to_ascii(&invalid);
    assert!(
        result.is_err(),
        "invalid element bytes should produce an error"
    );
}

#[test]
fn optional_element_hex_to_ascii_with_none() {
    let result = optional_element_hex_to_ascii(None).expect("None input should always succeed");
    assert_eq!(result, "None");
}

#[test]
fn optional_element_hex_to_ascii_with_some_valid() {
    let grove_version = GroveVersion::latest();
    let item = Element::new_item(b"opt_val".to_vec());
    let serialized = item
        .serialize(grove_version)
        .expect("should serialize item");

    let result = optional_element_hex_to_ascii(Some(&serialized))
        .expect("valid serialized element should deserialize");
    assert!(
        !result.is_empty(),
        "display should not be empty for valid element"
    );
}

#[test]
fn optional_element_hex_to_ascii_with_some_invalid() {
    let invalid = vec![0xFF, 0xFE];
    let result = optional_element_hex_to_ascii(Some(&invalid));
    assert!(result.is_err(), "invalid bytes should produce an error");
}

// ===========================================================================
// Additional lib.rs coverage — propagate_changes, root hash consistency
// ===========================================================================

#[test]
fn root_hash_changes_after_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    let hash_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash before");

    db.insert(
        [TEST_LEAF].as_ref(),
        b"new_key",
        Element::new_item(b"new_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    let hash_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash after");

    assert_ne!(
        hash_before, hash_after,
        "root hash should change after inserting data"
    );
}

#[test]
fn root_hash_changes_after_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.insert(
        [TEST_LEAF].as_ref(),
        b"delete_me",
        Element::new_item(b"will_be_deleted".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item to delete");

    let hash_before_delete = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash before delete");

    db.delete(
        [TEST_LEAF].as_ref(),
        b"delete_me",
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete item");

    let hash_after_delete = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash after delete");

    assert_ne!(
        hash_before_delete, hash_after_delete,
        "root hash should change after deleting data"
    );
}

#[test]
fn verify_grovedb_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    // Insert data within transaction
    db.insert(
        [TEST_LEAF].as_ref(),
        b"tx_verify_key",
        Element::new_item(b"tx_verify_value".to_vec()),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("should insert in transaction");

    // Verify within transaction
    let issues = db
        .verify_grovedb(Some(&tx), false, true, grove_version)
        .expect("verify_grovedb within transaction should succeed");
    assert!(
        issues.is_empty(),
        "transaction data should pass verification, got: {:?}",
        issues
    );
}

#[test]
fn batch_average_case_sum_tree_insert() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"sum_tree_key".to_vec(),
        Element::empty_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                12,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"sum_tree_key".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::SumTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 8, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case sum tree insert should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "sum tree insert should add bytes"
    );
}

#[test]
fn batch_worst_case_sum_tree_insert() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"wc_sum_tree".to_vec(),
        Element::empty_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case sum tree insert should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case sum tree insert should add bytes"
    );
}

#[test]
fn batch_worst_case_delete_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_tree_op(
        vec![b"leaf".to_vec()],
        b"child_tree".to_vec(),
        TreeType::NormalTree,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(20),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(20),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case delete tree cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case delete tree should have seeks"
    );
}

#[test]
fn batch_average_case_delete_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_tree_op(
        vec![b"leaf".to_vec()],
        b"child_tree".to_vec(),
        TreeType::NormalTree,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(20),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                10,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(20),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                10,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case delete tree cost should succeed");

    assert!(
        cost.seek_count > 0,
        "average case delete tree should have seeks"
    );
}

// ===========================================================================
// verify_grovedb with CountTree and CountSumTree
// ===========================================================================

#[test]
fn verify_grovedb_with_count_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert a count tree
    db.insert(
        [TEST_LEAF].as_ref(),
        b"count_tree",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert count tree");

    // Insert items into the count tree
    db.insert(
        [TEST_LEAF, b"count_tree"].as_ref(),
        b"c1",
        Element::new_item(b"cv1".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item into count tree");

    db.insert(
        [TEST_LEAF, b"count_tree"].as_ref(),
        b"c2",
        Element::new_item(b"cv2".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert second item into count tree");

    let issues = db
        .verify_grovedb(None, false, true, grove_version)
        .expect("verify_grovedb with count tree should succeed");
    assert!(
        issues.is_empty(),
        "count tree should pass verification, got: {:?}",
        issues
    );
}

#[test]
fn verify_grovedb_with_count_sum_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert a count sum tree
    db.insert(
        [TEST_LEAF].as_ref(),
        b"count_sum_tree",
        Element::empty_count_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert count sum tree");

    // Insert sum items into the count sum tree
    db.insert(
        [TEST_LEAF, b"count_sum_tree"].as_ref(),
        b"cs1",
        Element::new_sum_item(10),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert sum item into count sum tree");

    db.insert(
        [TEST_LEAF, b"count_sum_tree"].as_ref(),
        b"cs2",
        Element::new_sum_item(20),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert second sum item into count sum tree");

    let issues = db
        .verify_grovedb(None, false, true, grove_version)
        .expect("verify_grovedb with count sum tree should succeed");
    assert!(
        issues.is_empty(),
        "count sum tree should pass verification, got: {:?}",
        issues
    );
}

#[test]
fn verify_grovedb_with_nested_trees() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Create nested tree structure: TEST_LEAF -> tree_a -> tree_b -> item
    db.insert(
        [TEST_LEAF].as_ref(),
        b"tree_a",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree_a");

    db.insert(
        [TEST_LEAF, b"tree_a"].as_ref(),
        b"tree_b",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree_b");

    db.insert(
        [TEST_LEAF, b"tree_a", b"tree_b"].as_ref(),
        b"deep_item",
        Element::new_item(b"deep_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert deep item");

    // Also add sum tree in another branch
    db.insert(
        [TEST_LEAF].as_ref(),
        b"my_sum_tree",
        Element::empty_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert sum tree");

    db.insert(
        [TEST_LEAF, b"my_sum_tree"].as_ref(),
        b"s_item",
        Element::new_sum_item(42),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert sum item");

    let issues = db
        .verify_grovedb(None, true, true, grove_version)
        .expect("verify_grovedb with nested trees should succeed");
    assert!(
        issues.is_empty(),
        "nested trees should pass verification, got: {:?}",
        issues
    );
}

// ===========================================================================
// checkpoints coverage
// ===========================================================================

#[test]
fn checkpoint_create_and_open() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Insert data before checkpointing
    db.insert(
        [TEST_LEAF].as_ref(),
        b"ckpt_key",
        Element::new_item(b"ckpt_value".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item before checkpoint");

    let ckpt_dir = TempDir::new().expect("should create temp dir for checkpoint");
    let ckpt_path = ckpt_dir.path().join("checkpoint1");

    // Create checkpoint
    db.create_checkpoint(&ckpt_path)
        .expect("should create checkpoint");

    // Open checkpoint and verify data
    let ckpt_db = GroveDb::open_checkpoint(&ckpt_path).expect("should open checkpoint");
    let element = ckpt_db
        .get([TEST_LEAF].as_ref(), b"ckpt_key", None, grove_version)
        .unwrap()
        .expect("should get item from checkpoint");
    assert_eq!(
        element,
        Element::new_item(b"ckpt_value".to_vec()),
        "checkpoint should contain the data from the original DB"
    );

    // Verify checkpoint root hash matches original
    let original_hash = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get original root hash");
    let ckpt_hash = ckpt_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get checkpoint root hash");
    assert_eq!(
        original_hash, ckpt_hash,
        "checkpoint root hash should match original"
    );
}

#[test]
fn checkpoint_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    let ckpt_dir = TempDir::new().expect("should create temp dir for checkpoint");
    let ckpt_path = ckpt_dir.path().join("checkpoint_to_delete");

    db.create_checkpoint(&ckpt_path)
        .expect("should create checkpoint");

    assert!(ckpt_path.exists(), "checkpoint directory should exist");

    GroveDb::delete_checkpoint(&ckpt_path).expect("should delete checkpoint");

    assert!(
        !ckpt_path.exists(),
        "checkpoint directory should be deleted"
    );
}

#[test]
fn checkpoint_delete_short_path_rejected() {
    // Attempting to delete a very short path should fail the safety check
    let result = GroveDb::delete_checkpoint("/");
    assert!(result.is_err(), "deleting root path should be rejected");
}

// ===========================================================================
// batch estimated costs: additional GroveOp variants
// ===========================================================================

#[test]
fn batch_average_case_replace_with_tree() {
    let grove_version = GroveVersion::latest();

    // Replace an existing entry with a tree element
    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"tree_key".to_vec(),
        Element::empty_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case replace with tree should succeed");

    assert!(cost.seek_count > 0, "replace with tree should have seeks");
}

#[test]
fn batch_average_case_patch_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::patch_op(
        vec![b"leaf".to_vec()],
        b"patch_key".to_vec(),
        Element::new_item(b"patched_value".to_vec()),
        5,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(9, 32, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case patch cost should succeed");

    assert!(cost.seek_count > 0, "patch should have seeks");
}

#[test]
fn batch_average_case_refresh_reference_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
        vec![b"leaf".to_vec()],
        b"ref_key".to_vec(),
        crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
            b"leaf".to_vec(),
            b"target".to_vec(),
        ]),
        Some(10),
        Some(b"flags".to_vec()),
        true,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(7, 64, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case refresh reference cost should succeed");

    assert!(cost.seek_count > 0, "refresh reference should have seeks");
}

#[test]
fn batch_average_case_replace_sum_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"sum_item_key".to_vec(),
        Element::new_sum_item(999),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::SumTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(32, 8, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case replace sum item cost should succeed");

    assert!(cost.seek_count > 0, "replace sum item should have seeks");
}

#[test]
fn batch_average_case_replace_sum_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"sum_tree_replace".to_vec(),
        Element::empty_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(20),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                16,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case replace sum tree cost should succeed");

    assert!(cost.seek_count > 0, "replace sum tree should have seeks");
}

#[test]
fn batch_average_case_insert_into_sum_tree() {
    let grove_version = GroveVersion::latest();

    // Insert a tree element into a SumTree parent (tests InsertOrReplace with tree
    // in SumTree context)
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![b"sum_parent".to_vec()],
        b"sub_tree".to_vec(),
        Element::empty_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(5),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                10,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"sum_parent".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::SumTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case insert tree into sum tree should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "inserting tree into sum tree should add bytes"
    );
}

// ===========================================================================
// batch estimated costs: worst case for additional GroveOp variants
// ===========================================================================

#[test]
fn batch_worst_case_replace_with_tree() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"tree_key".to_vec(),
        Element::empty_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case replace with tree should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case replace with tree should have seeks"
    );
}

#[test]
fn batch_worst_case_patch_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::patch_op(
        vec![b"leaf".to_vec()],
        b"patch_key".to_vec(),
        Element::new_item(b"patched".to_vec()),
        3,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case patch cost should succeed");

    assert!(cost.seek_count > 0, "worst case patch should have seeks");
}

#[test]
fn batch_worst_case_refresh_reference_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
        vec![b"leaf".to_vec()],
        b"ref_key".to_vec(),
        crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
            b"leaf".to_vec(),
            b"target".to_vec(),
        ]),
        Some(10),
        Some(b"flags".to_vec()),
        true,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case refresh reference cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case refresh reference should have seeks"
    );
}

#[test]
fn batch_worst_case_replace_sum_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"sum_key".to_vec(),
        Element::new_sum_item(999),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(100),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case replace sum item cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case replace sum item should have seeks"
    );
}

#[test]
fn batch_worst_case_replace_sum_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"stree_key".to_vec(),
        Element::empty_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(20),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case replace sum tree cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case replace sum tree should have seeks"
    );
}

#[test]
fn batch_worst_case_delete_sum_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_tree_op(
        vec![b"leaf".to_vec()],
        b"sum_tree_del".to_vec(),
        TreeType::SumTree,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(20),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case delete sum tree cost should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case delete sum tree should have seeks"
    );
}

#[test]
fn batch_average_case_delete_sum_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_tree_op(
        vec![b"leaf".to_vec()],
        b"sum_tree_del".to_vec(),
        TreeType::SumTree,
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(20),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                16,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 1,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case delete sum tree cost should succeed");

    assert!(
        cost.seek_count > 0,
        "average case delete sum tree should have seeks"
    );
}

// ===========================================================================
// estimated_costs/average_case_costs.rs coverage: individual functions
// ===========================================================================

#[test]
fn average_case_get_merk_at_path_non_empty() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree1".to_vec())]);
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        false, // merk is NOT empty, triggers extra seek
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute average case get merk at path");

    // Non-empty merk should have 2 seeks (1 base + 1 for loading tree)
    assert!(
        cost.seek_count >= 2,
        "non-empty merk should have at least 2 seeks, got {}",
        cost.seek_count
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "should load bytes for non-root path"
    );
}

#[test]
fn average_case_get_merk_at_path_empty_root() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![]); // root path
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        true, // empty
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute average case get merk at root path");

    // Root path with empty merk should have 1 seek only
    assert_eq!(cost.seek_count, 1, "empty root merk should have 1 seek");
    assert_eq!(
        cost.storage_loaded_bytes, 0,
        "root path should load 0 bytes"
    );
}

#[test]
fn average_case_get_merk_at_path_sum_tree_type() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"sum_path".to_vec())]);
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        false,
        TreeType::SumTree,
        grove_version,
    )
    .expect("should compute average case get merk at path for sum tree");

    assert!(
        cost.seek_count >= 2,
        "non-empty sum tree path should have at least 2 seeks"
    );
}

#[test]
fn average_case_get_raw_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"mykey".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        64,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute average case get raw cost");

    assert!(
        cost.seek_count >= 1,
        "get raw should have at least 1 seek, got {}",
        cost.seek_count
    );
    assert!(cost.storage_loaded_bytes > 0, "get raw should load bytes");
}

#[test]
fn average_case_get_raw_tree_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"subtree_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        4, // estimated flags size
        TreeType::SumTree,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute average case get raw tree cost");

    assert!(
        cost.seek_count >= 1,
        "get raw tree should have at least 1 seek, got {}",
        cost.seek_count
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "get raw tree should load bytes"
    );
}

#[test]
fn average_case_has_raw_tree_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"check_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        4, // estimated flags size
        TreeType::CountTree,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute average case has raw tree cost");

    assert_eq!(cost.seek_count, 1, "has raw tree should have 1 seek");
    assert!(
        cost.storage_loaded_bytes > 0,
        "has raw tree should load bytes"
    );
}

#[test]
fn average_case_get_cost_with_references() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"ref_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        TreeType::NormalTree,
        64,
        vec![128, 256], // two reference hops
        grove_version,
    )
    .expect("should compute average case get cost with references");

    // 1 base seek + 2 reference hops = 3
    assert_eq!(
        cost.seek_count, 3,
        "get cost with 2 references should have 3 seeks"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "should load bytes for element + references"
    );
}

// ===========================================================================
// estimated_costs/worst_case_costs.rs coverage: individual functions
// ===========================================================================

#[test]
fn worst_case_get_merk_at_path_non_root() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree1".to_vec())]);
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute worst case get merk at path");

    assert_eq!(
        cost.seek_count, 2,
        "worst case merk at path should have 2 seeks"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "worst case should load bytes for non-root path"
    );
}

#[test]
fn worst_case_get_merk_at_path_root() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![]);
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute worst case get merk at root path");

    assert_eq!(cost.seek_count, 2, "worst case always has 2 seeks");
    // Root path has no key, so storage_loaded_bytes only from storage context cost
}

#[test]
fn worst_case_get_raw_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"mykey".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        128,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute worst case get raw cost");

    assert!(
        cost.seek_count >= 1,
        "worst case get raw should have at least 1 seek, got {}",
        cost.seek_count
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "worst case get raw should load bytes"
    );
}

#[test]
fn worst_case_get_raw_tree_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"subtree_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_raw_tree_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        TreeType::SumTree,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute worst case get raw tree cost");

    assert!(
        cost.seek_count >= 1,
        "worst case get raw tree should have at least 1 seek, got {}",
        cost.seek_count
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "worst case get raw tree should load bytes"
    );
}

#[test]
fn worst_case_get_cost_with_references() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"ref_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        64,
        TreeType::NormalTree,
        vec![128, 256], // two reference hops
        grove_version,
    )
    .expect("should compute worst case get cost with references");

    // 1 base seek + 2 reference hops = 3
    assert_eq!(
        cost.seek_count, 3,
        "worst case get cost with 2 references should have 3 seeks"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "should load bytes for element + references"
    );
}

#[test]
fn worst_case_has_raw_cost_function() {
    use grovedb_costs::OperationCost;
    use grovedb_storage::rocksdb_storage::RocksDbStorage;

    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath(vec![KeyInfo::KnownKey(b"tree".to_vec())]);
    let key = KeyInfo::KnownKey(b"check_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        128,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should compute worst case has raw cost");

    assert_eq!(cost.seek_count, 1, "worst case has raw should have 1 seek");
    assert!(
        cost.storage_loaded_bytes > 0,
        "worst case has raw should load bytes"
    );
}

// ===========================================================================
// batch estimated costs: insert with count trees
// ===========================================================================

#[test]
fn batch_average_case_insert_count_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"count_tree_key".to_vec(),
        Element::empty_count_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                14,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 1,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"count_tree_key".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::CountTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case count tree insert should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "count tree insert should add bytes"
    );
}

#[test]
fn batch_worst_case_insert_count_tree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"count_tree_key".to_vec(),
        Element::empty_count_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case count tree insert should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case count tree insert should add bytes"
    );
}

// ===========================================================================
// batch estimated costs: insert only variant
// ===========================================================================

#[test]
fn batch_average_case_insert_only_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_only_op(
        vec![],
        b"insert_only_key".to_vec(),
        Element::new_item(b"insert_only_value".to_vec()),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(5),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(15, 17, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case insert only cost should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "insert only should add bytes"
    );
}

#[test]
fn batch_worst_case_insert_only_item_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_only_op(
        vec![],
        b"insert_only_key".to_vec(),
        Element::new_item(b"insert_only_value".to_vec()),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(5),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case insert only cost should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case insert only should add bytes"
    );
}

// ===========================================================================
// Additional batch estimated cost tests for edge cases
// ===========================================================================

#[test]
fn batch_average_case_delete_in_subtree_cost() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::delete_op(
        vec![b"leaf".to_vec()],
        b"item_to_delete".to_vec(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                4,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(14, 32, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case delete in subtree should succeed");

    assert!(cost.seek_count > 0, "delete in subtree should have seeks");
}

#[test]
fn batch_average_case_insert_reference_element() {
    let grove_version = GroveVersion::latest();

    // Insert a reference element (non-tree, non-item)
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![b"leaf".to_vec()],
        b"ref_element".to_vec(),
        Element::new_reference(
            crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
                b"leaf".to_vec(),
                b"target".to_vec(),
            ]),
        ),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(5),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                4,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(20),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(11, 64, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case insert reference should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "insert reference should add bytes"
    );
}

#[test]
fn batch_average_case_replace_item_with_flags() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"flagged_item".to_vec(),
        Element::new_item_with_flags(b"new_val".to_vec(), Some(b"myflags".to_vec())),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                4,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(12, 32, Some(7)),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case replace item with flags should succeed");

    assert!(
        cost.seek_count > 0,
        "replace item with flags should have seeks"
    );
}

#[test]
fn batch_worst_case_replace_item_general() {
    let grove_version = GroveVersion::latest();

    // Replace with a general element (Reference) to trigger the _ arm
    let ops = vec![QualifiedGroveDbOp::replace_op(
        vec![b"leaf".to_vec()],
        b"general_key".to_vec(),
        Element::new_reference(
            crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
                b"leaf".to_vec(),
                b"target".to_vec(),
            ]),
        ),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"leaf".to_vec())]),
        WorstCaseLayerInformation::MaxElementsNumber(50),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case replace with reference should succeed");

    assert!(
        cost.seek_count > 0,
        "worst case replace with reference should have seeks"
    );
}

#[test]
fn batch_worst_case_insert_tree_with_flags() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"flagged_tree".to_vec(),
        Element::empty_tree_with_flags(Some(b"flags".to_vec())),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case insert tree with flags should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case insert tree with flags should add bytes"
    );
}

#[test]
fn batch_worst_case_insert_big_sum_tree() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"big_sum".to_vec(),
        Element::empty_big_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        WorstCaseLayerInformation::MaxElementsNumber(10),
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        WorstCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("worst case insert big sum tree should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "worst case insert big sum tree should add bytes"
    );
}

#[test]
fn batch_average_case_insert_big_sum_tree() {
    let grove_version = GroveVersion::latest();

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"big_sum".to_vec(),
        Element::empty_big_sum_tree(),
    )];

    let mut paths = HashMap::new();
    paths.insert(
        KeyInfoPath(vec![]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(10),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                7,
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 1,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 1,
                },
                None,
            ),
        },
    );
    paths.insert(
        KeyInfoPath(vec![KeyInfo::KnownKey(b"big_sum".to_vec())]),
        EstimatedLayerInformation {
            tree_type: TreeType::BigSumTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(0),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        },
    );

    let cost = GroveDb::estimated_case_operations_for_batch(
        AverageCaseCostsType(paths),
        ops,
        None,
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| Ok((NoStorageRemoval, NoStorageRemoval)),
        grove_version,
    )
    .cost_as_result()
    .expect("average case insert big sum tree should succeed");

    assert!(
        cost.storage_cost.added_bytes > 0,
        "average case insert big sum tree should add bytes"
    );
}
