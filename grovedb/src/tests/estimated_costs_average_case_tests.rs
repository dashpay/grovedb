//! Tests for average case cost estimation functions
//!
//! These tests validate that the average case cost estimation functions in
//! `grovedb/src/estimated_costs/average_case_costs.rs` produce non-trivial
//! cost values for valid inputs.

use grovedb_costs::OperationCost;
use grovedb_merk::{
    estimated_costs::average_case_costs::{
        EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes,
    },
    tree_type::TreeType,
};
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
    Element, GroveDb,
};

/// Helper to build a standard `EstimatedLayerInformation` for a normal tree
/// with approximately 100 elements and 8-byte keys.
fn normal_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

/// Helper to build an `EstimatedLayerInformation` for a sum tree.
fn sum_tree_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::SumTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

/// Helper to build an `EstimatedLayerInformation` with all-items sizing.
fn items_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(200),
        estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 100, None),
    }
}

// ---------------------------------------------------------------------------
// average_case_merk_replace_tree
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_replace_tree_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"my_tree_key".to_vec());
    let layer_info = normal_layer_info();

    let result = GroveDb::average_case_merk_replace_tree(
        &key,
        &layer_info,
        TreeType::NormalTree,
        false,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("replace tree should succeed without propagation");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_loaded_bytes > 0 || cost.hash_node_calls > 0,
        "replace tree cost should be non-trivial: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_replace_tree_with_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"propagated_key".to_vec());
    let layer_info = normal_layer_info();

    let without_propagation = GroveDb::average_case_merk_replace_tree(
        &key,
        &layer_info,
        TreeType::NormalTree,
        false,
        grove_version,
    );
    let with_propagation = GroveDb::average_case_merk_replace_tree(
        &key,
        &layer_info,
        TreeType::NormalTree,
        true,
        grove_version,
    );
    without_propagation
        .value
        .as_ref()
        .expect("replace tree without propagation should succeed");
    with_propagation
        .value
        .as_ref()
        .expect("replace tree with propagation should succeed");

    // Propagation should add extra cost
    assert!(
        with_propagation.cost.seek_count > without_propagation.cost.seek_count
            || with_propagation.cost.hash_node_calls > without_propagation.cost.hash_node_calls,
        "propagation should increase cost"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_insert_tree
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_insert_tree_no_flags_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"new_tree".to_vec());
    let flags: Option<Vec<u8>> = None;

    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &flags,
        TreeType::NormalTree,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result.value.as_ref().expect("insert tree should succeed");
    let cost = result.cost;
    assert!(
        cost.storage_cost.added_bytes > 0,
        "inserting a tree should add bytes: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_insert_tree_with_flags_and_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"flagged_tree".to_vec());
    let flags: Option<Vec<u8>> = Some(vec![1, 2, 3, 4]);
    let layer_info = normal_layer_info();

    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &flags,
        TreeType::NormalTree,
        TreeType::NormalTree,
        Some(&layer_info),
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("insert tree with flags should succeed");
    let cost = result.cost;
    assert!(
        cost.storage_cost.added_bytes > 0 && cost.seek_count > 0,
        "insert with propagation should produce meaningful cost: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_insert_sum_tree() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"sum_tree".to_vec());
    let flags: Option<Vec<u8>> = None;

    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &flags,
        TreeType::SumTree,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("insert sum tree should succeed");
    assert!(
        result.cost.storage_cost.added_bytes > 0,
        "sum tree insert should add bytes"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_delete_tree
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_delete_tree_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"delete_me".to_vec());
    let layer_info = normal_layer_info();

    let result = GroveDb::average_case_merk_delete_tree(
        &key,
        TreeType::NormalTree,
        &layer_info,
        false,
        grove_version,
    );
    result.value.as_ref().expect("delete tree should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.hash_node_calls > 0,
        "delete tree should have non-zero cost: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_delete_tree_with_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"delete_prop".to_vec());
    let layer_info = normal_layer_info();

    let without = GroveDb::average_case_merk_delete_tree(
        &key,
        TreeType::NormalTree,
        &layer_info,
        false,
        grove_version,
    );
    let with = GroveDb::average_case_merk_delete_tree(
        &key,
        TreeType::NormalTree,
        &layer_info,
        true,
        grove_version,
    );
    with.value
        .as_ref()
        .expect("delete tree with propagation should succeed");
    assert!(
        with.cost.seek_count > without.cost.seek_count
            || with.cost.hash_node_calls > without.cost.hash_node_calls,
        "propagation should increase delete cost"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_insert_element
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_insert_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"item_key".to_vec());
    let value = Element::new_item(b"hello world".to_vec());

    let result = GroveDb::average_case_merk_insert_element(
        &key,
        &value,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("insert item element should succeed");
    assert!(
        result.cost.storage_cost.added_bytes > 0,
        "inserting an item should add bytes"
    );
}

#[test]
fn test_average_case_merk_insert_tree_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"tree_elem".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::average_case_merk_insert_element(
        &key,
        &value,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("insert tree element should succeed");
    assert!(
        result.cost.storage_cost.added_bytes > 0,
        "inserting a tree element should add bytes"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_replace_element
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_replace_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_item".to_vec());
    let value = Element::new_item(b"replacement".to_vec());

    let result = GroveDb::average_case_merk_replace_element(
        &key,
        &value,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("replace item element should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_cost.replaced_bytes > 0,
        "replacing an item should have non-trivial cost: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_replace_tree_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_tree".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::average_case_merk_replace_element(
        &key,
        &value,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("replace tree element should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_cost.replaced_bytes > 0,
        "replacing a tree element should have non-trivial cost: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_patch_element
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_patch_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"patch_key".to_vec());
    let value = Element::new_item(b"patchable".to_vec());

    let result = GroveDb::average_case_merk_patch_element(
        &key,
        &value,
        5,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    result
        .value
        .as_ref()
        .expect("patching an Item should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_cost.replaced_bytes > 0,
        "patching should produce non-trivial cost: {cost:?}"
    );
}

#[test]
fn test_average_case_merk_patch_non_item_returns_error() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"patch_tree".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::average_case_merk_patch_element(
        &key,
        &value,
        5,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    assert!(
        result.value.is_err(),
        "patching a non-Item element should return an error"
    );
}

// ---------------------------------------------------------------------------
// average_case_merk_delete_element
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_merk_delete_element_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"del_elem".to_vec());
    let layer_info = items_layer_info();

    let result = GroveDb::average_case_merk_delete_element(&key, &layer_info, false, grove_version);
    result
        .value
        .as_ref()
        .expect("delete element should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.hash_node_calls > 0,
        "delete element should have non-zero cost: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_average_case_get_merk_at_path
// ---------------------------------------------------------------------------

#[test]
fn test_add_average_case_get_merk_at_path_non_empty() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"root".to_vec()),
        KnownKey(b"subtree".to_vec()),
    ]);
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        false,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed for non-empty merk");

    assert!(
        cost.seek_count >= 2,
        "non-empty merk should have at least 2 seeks: {cost:?}"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "should load bytes when path is non-empty: {cost:?}"
    );
}

#[test]
fn test_add_average_case_get_merk_at_path_empty() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        true,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed for empty merk");

    // Empty merk only seeks once (no root node load)
    assert!(
        cost.seek_count == 1,
        "empty merk should have exactly 1 seek: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_average_case_has_raw_tree_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_average_case_has_raw_tree_cost() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"subtree_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        4, // estimated_flags_size
        TreeType::NormalTree,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed");

    assert!(cost.seek_count > 0, "has_raw_tree should seek: {cost:?}");
    assert!(
        cost.storage_loaded_bytes > 0,
        "has_raw_tree should load bytes: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_average_case_get_raw_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_average_case_get_raw_cost() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"raw_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        100, // estimated_element_size
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed");

    assert!(cost.seek_count > 0, "get_raw should seek: {cost:?}");
    assert!(
        cost.storage_loaded_bytes > 0,
        "get_raw should load bytes: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_average_case_get_raw_tree_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_average_case_get_raw_tree_cost() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"tree_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        0, // estimated_flags_size
        TreeType::NormalTree,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed");

    assert!(cost.seek_count > 0, "get_raw_tree should seek: {cost:?}");
    assert!(
        cost.storage_loaded_bytes > 0,
        "get_raw_tree should load bytes: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_average_case_get_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_average_case_get_cost_no_references() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"get_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        TreeType::NormalTree,
        100,    // estimated_element_size
        vec![], // no references
        grove_version,
    )
    .expect("should succeed");

    assert!(
        cost.seek_count == 1,
        "get with no references should seek once: {cost:?}"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "get should load bytes: {cost:?}"
    );
}

#[test]
fn test_add_average_case_get_cost_with_references() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"ref_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_average_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        TreeType::NormalTree,
        100,
        vec![50, 60], // two reference hops
        grove_version,
    )
    .expect("should succeed");

    assert!(
        cost.seek_count == 3,
        "get with 2 references should seek 3 times: {cost:?}"
    );
    assert!(
        cost.storage_loaded_bytes > 100,
        "references should add to loaded bytes: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// Cross-cutting: sum tree vs normal tree cost differences
// ---------------------------------------------------------------------------

#[test]
fn test_average_case_replace_tree_sum_vs_normal() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"compare_key".to_vec());
    let normal_info = normal_layer_info();
    let sum_info = sum_tree_layer_info();

    let normal_cost = GroveDb::average_case_merk_replace_tree(
        &key,
        &normal_info,
        TreeType::NormalTree,
        false,
        grove_version,
    );
    let sum_cost = GroveDb::average_case_merk_replace_tree(
        &key,
        &sum_info,
        TreeType::SumTree,
        false,
        grove_version,
    );

    normal_cost
        .value
        .as_ref()
        .expect("normal tree replace should succeed");
    sum_cost
        .value
        .as_ref()
        .expect("sum tree replace should succeed");

    // Both should produce valid costs; sum trees have additional aggregation data
    // so they may differ in replaced_bytes
    assert!(
        normal_cost.cost.seek_count > 0 && sum_cost.cost.seek_count > 0,
        "both should have non-zero seek count"
    );
}
