//! Tests for worst case cost estimation functions
//!
//! These tests validate that the worst case cost estimation functions in
//! `grovedb/src/estimated_costs/worst_case_costs.rs` produce non-trivial
//! cost values for valid inputs.

use grovedb_costs::OperationCost;
use grovedb_merk::{
    estimated_costs::worst_case_costs::WorstCaseLayerInformation, tree_type::TreeType,
};
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use crate::{
    Element, GroveDb,
    batch::{KeyInfoPath, key_info::KeyInfo::KnownKey},
};

/// Helper for a standard worst-case layer with 1000 elements.
fn standard_worst_case_info() -> WorstCaseLayerInformation {
    WorstCaseLayerInformation::MaxElementsNumber(1000)
}

/// Helper for a small worst-case layer with 10 elements.
fn small_worst_case_info() -> WorstCaseLayerInformation {
    WorstCaseLayerInformation::MaxElementsNumber(10)
}

// ---------------------------------------------------------------------------
// worst_case_merk_replace_tree
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_replace_tree_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_key".to_vec());
    let layer_info = standard_worst_case_info();

    let result = GroveDb::worst_case_merk_replace_tree(
        &key,
        TreeType::NormalTree,
        TreeType::NormalTree,
        &layer_info,
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
fn test_worst_case_merk_replace_tree_with_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_prop".to_vec());
    let layer_info = standard_worst_case_info();

    let without = GroveDb::worst_case_merk_replace_tree(
        &key,
        TreeType::NormalTree,
        TreeType::NormalTree,
        &layer_info,
        false,
        grove_version,
    );
    let with = GroveDb::worst_case_merk_replace_tree(
        &key,
        TreeType::NormalTree,
        TreeType::NormalTree,
        &layer_info,
        true,
        grove_version,
    );
    without
        .value
        .as_ref()
        .expect("replace tree without propagation should succeed");
    with.value
        .as_ref()
        .expect("replace tree with propagation should succeed");

    assert!(
        with.cost.seek_count > without.cost.seek_count
            || with.cost.hash_node_calls > without.cost.hash_node_calls,
        "propagation should increase cost"
    );
}

// ---------------------------------------------------------------------------
// worst_case_merk_insert_tree
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_insert_tree_no_flags_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"insert_tree".to_vec());
    let flags: Option<Vec<u8>> = None;

    let result = GroveDb::worst_case_merk_insert_tree(
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
fn test_worst_case_merk_insert_tree_with_flags_and_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"flagged_tree".to_vec());
    let flags: Option<Vec<u8>> = Some(vec![0xAA, 0xBB, 0xCC]);
    let layer_info = standard_worst_case_info();

    let result = GroveDb::worst_case_merk_insert_tree(
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
fn test_worst_case_merk_insert_sum_tree() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"sum_tree".to_vec());
    let flags: Option<Vec<u8>> = None;

    let result = GroveDb::worst_case_merk_insert_tree(
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
// worst_case_merk_delete_tree
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_delete_tree_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"delete_me".to_vec());
    let layer_info = standard_worst_case_info();

    let result = GroveDb::worst_case_merk_delete_tree(
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
fn test_worst_case_merk_delete_tree_with_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"delete_prop".to_vec());
    let layer_info = standard_worst_case_info();

    let without = GroveDb::worst_case_merk_delete_tree(
        &key,
        TreeType::NormalTree,
        &layer_info,
        false,
        grove_version,
    );
    let with = GroveDb::worst_case_merk_delete_tree(
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
// worst_case_merk_insert_element
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_insert_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"item_key".to_vec());
    let value = Element::new_item(b"some value data".to_vec());

    let result = GroveDb::worst_case_merk_insert_element(
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
fn test_worst_case_merk_insert_tree_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"tree_elem".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::worst_case_merk_insert_element(
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
// worst_case_merk_replace_element
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_replace_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_item".to_vec());
    let value = Element::new_item(b"replacement data".to_vec());

    let result = GroveDb::worst_case_merk_replace_element(
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
fn test_worst_case_merk_replace_tree_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_tree".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::worst_case_merk_replace_element(
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
// worst_case_merk_patch_element
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_patch_item_element() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"patch_key".to_vec());
    let value = Element::new_item(b"patchable data".to_vec());

    let result = GroveDb::worst_case_merk_patch_element(
        &key,
        &value,
        10,
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
fn test_worst_case_merk_patch_non_item_returns_error() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"patch_tree".to_vec());
    let value = Element::empty_tree();

    let result = GroveDb::worst_case_merk_patch_element(
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
// worst_case_merk_delete_element
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_merk_delete_element_no_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"del_elem".to_vec());
    let layer_info = standard_worst_case_info();

    let result = GroveDb::worst_case_merk_delete_element(&key, &layer_info, false, grove_version);
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

#[test]
fn test_worst_case_merk_delete_element_with_propagate() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"del_elem_prop".to_vec());
    let layer_info = standard_worst_case_info();

    let without = GroveDb::worst_case_merk_delete_element(&key, &layer_info, false, grove_version);
    let with = GroveDb::worst_case_merk_delete_element(&key, &layer_info, true, grove_version);
    with.value
        .as_ref()
        .expect("delete element with propagation should succeed");
    assert!(
        with.cost.seek_count > without.cost.seek_count
            || with.cost.hash_node_calls > without.cost.hash_node_calls,
        "propagation should increase delete element cost"
    );
}

// ---------------------------------------------------------------------------
// add_worst_case_get_merk_at_path
// ---------------------------------------------------------------------------

#[test]
fn test_add_worst_case_get_merk_at_path() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"root".to_vec()),
        KnownKey(b"child".to_vec()),
    ]);
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
        &mut cost,
        &path,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed");

    assert!(
        cost.seek_count >= 2,
        "worst case merk open should have at least 2 seeks: {cost:?}"
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "should load bytes when path is non-empty: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_worst_case_has_raw_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_worst_case_has_raw_cost() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"check_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        100, // max_element_size
        TreeType::NormalTree,
        grove_version,
    )
    .expect("should succeed");

    assert!(cost.seek_count > 0, "has_raw should seek: {cost:?}");
    assert!(
        cost.storage_loaded_bytes > 0,
        "has_raw should load bytes: {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// add_worst_case_get_raw_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_worst_case_get_raw_cost() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"raw_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        200, // max_element_size
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
// add_worst_case_get_cost
// ---------------------------------------------------------------------------

#[test]
fn test_add_worst_case_get_cost_no_references() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"get_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        100, // max_element_size
        TreeType::NormalTree,
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
fn test_add_worst_case_get_cost_with_references() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"ref_key".to_vec());
    let mut cost = OperationCost::default();

    GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
        &mut cost,
        &path,
        &key,
        100,
        TreeType::NormalTree,
        vec![80, 90], // two reference hops
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
// Cross-cutting: worst case should be >= average case
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_has_raw_greater_than_or_equal_average_case() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"compare_key".to_vec());
    let element_size = 100_u32;

    let mut avg_cost = OperationCost::default();
    GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
        &mut avg_cost,
        &path,
        &key,
        element_size,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("average case should succeed");

    let mut worst_cost = OperationCost::default();
    GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
        &mut worst_cost,
        &path,
        &key,
        element_size,
        TreeType::NormalTree,
        grove_version,
    )
    .expect("worst case should succeed");

    assert!(
        worst_cost.storage_loaded_bytes >= avg_cost.storage_loaded_bytes,
        "worst case loaded bytes ({}) should be >= average case ({})",
        worst_cost.storage_loaded_bytes,
        avg_cost.storage_loaded_bytes
    );
}

// ---------------------------------------------------------------------------
// Element size differences: small vs large layers
// ---------------------------------------------------------------------------

#[test]
fn test_worst_case_delete_element_small_vs_large_layer() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"layer_compare".to_vec());
    let small_info = small_worst_case_info();
    let large_info = standard_worst_case_info();

    let small_result =
        GroveDb::worst_case_merk_delete_element(&key, &small_info, true, grove_version);
    let large_result =
        GroveDb::worst_case_merk_delete_element(&key, &large_info, true, grove_version);

    small_result
        .value
        .as_ref()
        .expect("small layer delete should succeed");
    large_result
        .value
        .as_ref()
        .expect("large layer delete should succeed");

    // A larger tree should have more propagation cost
    assert!(
        large_result.cost.hash_node_calls >= small_result.cost.hash_node_calls,
        "larger layer should have >= hash calls: large={}, small={}",
        large_result.cost.hash_node_calls,
        small_result.cost.hash_node_calls
    );
}
