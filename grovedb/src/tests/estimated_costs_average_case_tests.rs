//! Tests for average case cost estimation functions
//!
//! These tests validate that the average case cost estimation functions in
//! `grovedb/src/estimated_costs/average_case_costs.rs` produce non-trivial
//! cost values for valid inputs.

use grovedb_costs::OperationCost;
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    estimated_costs::{
        add_cost_case_merk_replace_layered,
        average_case_costs::{EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes},
    },
    tree_type::{TreeType, COUNT_BIG_SUM_TREE_COST_SIZE},
};
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

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
        0,
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
        0,
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
        0,
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

    GroveDb::add_average_case_get_merk_at_path_engine_free(
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

    GroveDb::add_average_case_get_merk_at_path_engine_free(
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

    GroveDb::add_average_case_has_raw_tree_cost_engine_free(
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

    GroveDb::add_average_case_get_raw_cost_engine_free(
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

    GroveDb::add_average_case_get_raw_tree_cost_engine_free(
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

    GroveDb::add_average_case_get_cost_engine_free(
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

    GroveDb::add_average_case_get_cost_engine_free(
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

// ---------------------------------------------------------------------------
// Indexed-tree type variants — exercises the per-variant cost_size() arms
// for PSIT/PCIT/PCPSIT in average_case_merk_insert_tree /
// average_case_merk_replace_tree / average_case_merk_delete_tree.
// ---------------------------------------------------------------------------

fn indexed_layer_info(tt: TreeType) -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: tt,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

#[test]
fn test_average_case_merk_insert_psit() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"psit".to_vec());
    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &None,
        TreeType::ProvableSumIndexedTree,
        TreeType::NormalTree,
        0,
        None,
        grove_version,
    );
    result.value.as_ref().expect("insert PSIT should succeed");
    assert!(
        result.cost.storage_cost.added_bytes > 0,
        "PSIT insert should add bytes"
    );
}

#[test]
fn test_average_case_merk_insert_pcit() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"pcit".to_vec());
    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &None,
        TreeType::ProvableCountIndexedTree,
        TreeType::NormalTree,
        0,
        None,
        grove_version,
    );
    result.value.as_ref().expect("insert PCIT should succeed");
}

#[test]
fn test_average_case_merk_insert_pcpsit() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"pcpsit".to_vec());
    let result = GroveDb::average_case_merk_insert_tree(
        &key,
        &None,
        TreeType::ProvableCountProvableSumIndexedTree,
        TreeType::NormalTree,
        0,
        None,
        grove_version,
    );
    result.value.as_ref().expect("insert PCPSIT should succeed");
}

#[test]
fn test_average_case_merk_replace_tree_indexed_variants() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"replace_indexed".to_vec());
    for tt in [
        TreeType::ProvableSumIndexedTree,
        TreeType::ProvableCountIndexedTree,
        TreeType::ProvableCountProvableSumIndexedTree,
    ] {
        let info = indexed_layer_info(tt);
        let result = GroveDb::average_case_merk_replace_tree(&key, &info, tt, true, grove_version);
        result
            .value
            .as_ref()
            .unwrap_or_else(|_| panic!("replace {:?} should succeed", tt));
        assert!(
            result.cost.seek_count > 0 || result.cost.hash_node_calls > 0,
            "{:?} replace cost should be non-trivial",
            tt
        );
    }
}

/// Creating a `ProvableCountProvableBigSumTree` is estimated, in the average
/// and the worst case, 7 bytes above the narrow tree: its element carries a
/// 16-byte sum where the narrow one carries 9.
#[test]
fn test_estimates_size_a_provable_count_provable_big_sum_tree() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"tree".to_vec());

    let average = |tree_type| {
        let result = GroveDb::average_case_merk_insert_tree(
            &key,
            &None,
            tree_type,
            TreeType::NormalTree,
            0,
            None,
            grove_version,
        );
        result.value.as_ref().expect("the estimate should succeed");
        result.cost.storage_cost.added_bytes
    };
    let worst = |tree_type| {
        let result = GroveDb::worst_case_merk_insert_tree(
            &key,
            &None,
            tree_type,
            TreeType::NormalTree,
            0,
            None,
            grove_version,
        );
        result.value.as_ref().expect("the estimate should succeed");
        result.cost.storage_cost.added_bytes
    };

    assert_eq!(
        average(TreeType::ProvableCountProvableBigSumTree),
        average(TreeType::ProvableCountProvableSumTree) + 7
    );
    assert_eq!(
        worst(TreeType::ProvableCountProvableBigSumTree),
        worst(TreeType::ProvableCountProvableSumTree) + 7
    );
}

/// Inserting or replacing a `ProvableCountProvableBigSumTree` element is
/// estimated as the layered tree it is written as, at its own cost size, by
/// the element estimators as well as the tree ones.
#[test]
fn test_element_estimates_treat_a_provable_count_provable_big_sum_tree_as_layered() {
    let grove_version = GroveVersion::latest();
    let key = KnownKey(b"tree".to_vec());
    let key_len = 4;
    let element = Element::empty_provable_count_provable_big_sum_tree();
    let tree_type = TreeType::ProvableCountProvableBigSumTree;

    let average_insert = GroveDb::average_case_merk_insert_element(
        &key,
        &element,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    average_insert
        .value
        .as_ref()
        .expect("the estimate should succeed");
    let average_insert_tree = GroveDb::average_case_merk_insert_tree(
        &key,
        &None,
        tree_type,
        TreeType::NormalTree,
        0,
        None,
        grove_version,
    );
    assert_eq!(average_insert.cost, average_insert_tree.cost);

    let worst_insert = GroveDb::worst_case_merk_insert_element(
        &key,
        &element,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    worst_insert
        .value
        .as_ref()
        .expect("the estimate should succeed");
    let worst_insert_tree = GroveDb::worst_case_merk_insert_tree(
        &key,
        &None,
        tree_type,
        TreeType::NormalTree,
        0,
        None,
        grove_version,
    );
    assert_eq!(worst_insert.cost, worst_insert_tree.cost);

    let mut layered_replace = OperationCost::default();
    add_cost_case_merk_replace_layered(
        &mut layered_replace,
        key_len,
        COUNT_BIG_SUM_TREE_COST_SIZE,
        TreeType::NormalTree,
    );
    let average_replace = GroveDb::average_case_merk_replace_element(
        &key,
        &element,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    average_replace
        .value
        .as_ref()
        .expect("the estimate should succeed");
    assert_eq!(average_replace.cost, layered_replace);
    let worst_replace = GroveDb::worst_case_merk_replace_element(
        &key,
        &element,
        TreeType::NormalTree,
        None,
        grove_version,
    );
    worst_replace
        .value
        .as_ref()
        .expect("the estimate should succeed");
    assert_eq!(worst_replace.cost, layered_replace);
}

/// A tree in each wrapper, next to the same tree without it. `NotSummed` and
/// `NotCountedOrSummed` only wrap sum-bearing trees.
fn wrapped_trees() -> Vec<(Element, Element)> {
    let flags = Some(vec![1, 2, 3]);
    let non_counted = |tree: Element| {
        let wrapped = Element::new_non_counted(tree.clone()).expect("a tree wraps as non-counted");
        (wrapped, tree)
    };
    let not_summed = |tree: Element| {
        let wrapped =
            Element::new_not_summed(tree.clone()).expect("a sum tree wraps as not-summed");
        (wrapped, tree)
    };
    let not_counted_or_summed = |tree: Element| {
        let wrapped = Element::new_not_counted_or_summed(tree.clone())
            .expect("a sum tree wraps as not-counted-or-summed");
        (wrapped, tree)
    };
    vec![
        non_counted(Element::empty_tree()),
        non_counted(Element::empty_tree_with_flags(flags.clone())),
        non_counted(Element::empty_count_tree()),
        non_counted(Element::empty_provable_count_sum_tree()),
        not_summed(Element::empty_sum_tree()),
        not_summed(Element::empty_big_sum_tree_with_flags(flags.clone())),
        not_summed(Element::empty_count_sum_tree()),
        not_counted_or_summed(Element::empty_sum_tree()),
        not_counted_or_summed(Element::empty_provable_count_provable_sum_tree_with_flags(
            flags,
        )),
    ]
}

/// The average-case cost of inserting `element` under a count and sum tree,
/// which accepts all three wrappers, propagating through `level` if given.
fn average_insert_element_cost(
    element: &Element,
    level: Option<&EstimatedLayerInformation>,
    grove_version: &GroveVersion,
) -> OperationCost {
    let result = GroveDb::average_case_merk_insert_element(
        &KnownKey(b"tree".to_vec()),
        element,
        TreeType::CountSumTree,
        level,
        grove_version,
    );
    result.value.as_ref().expect("the estimate should succeed");
    result.cost
}

/// The average-case cost the dedicated tree helper gives the tree `element`
/// holds, with `wrapper_overhead` bytes for a wrapper.
fn average_insert_tree_cost(
    element: &Element,
    wrapper_overhead: u32,
    level: Option<&EstimatedLayerInformation>,
    grove_version: &GroveVersion,
) -> OperationCost {
    let (flags, tree_type) = element
        .tree_flags_and_type()
        .expect("the element should be a tree");
    let result = GroveDb::average_case_merk_insert_tree(
        &KnownKey(b"tree".to_vec()),
        flags,
        tree_type,
        TreeType::CountSumTree,
        wrapper_overhead,
        level,
        grove_version,
    );
    result.value.as_ref().expect("the estimate should succeed");
    result.cost
}

/// From `GROVE_V5` the element estimate sizes a wrapped tree as the dedicated
/// tree helper does with one byte of wrapper overhead: exactly one byte above
/// the same tree without its wrapper, for each of the three wrappers, and
/// nothing for a tree that is not wrapped (issue #911).
#[test]
fn test_average_case_merk_insert_element_sizes_a_wrapped_tree_with_its_wrapper_byte() {
    let grove_version = GroveVersion::latest();
    let layer_info = normal_layer_info();
    for (wrapped, bare) in wrapped_trees() {
        for level in [None, Some(&layer_info)] {
            let wrapped_cost = average_insert_element_cost(&wrapped, level, grove_version);
            let bare_cost = average_insert_element_cost(&bare, level, grove_version);
            assert_eq!(
                wrapped_cost,
                average_insert_tree_cost(&wrapped, 1, level, grove_version),
                "{wrapped}: must be estimated as the tree helper estimates a wrapped tree",
            );
            assert_eq!(
                bare_cost,
                average_insert_tree_cost(&bare, 0, level, grove_version),
                "{bare}: must be estimated as the tree helper estimates an unwrapped tree",
            );
            assert_eq!(
                wrapped_cost.storage_cost.added_bytes,
                bare_cost.storage_cost.added_bytes + 1,
                "{wrapped}: the wrapper must add exactly one byte",
            );
        }
    }
}

/// Replay guarantee: `GROVE_V4` is live and sizes a wrapped tree as the tree
/// alone, without its wrapper's byte. Pinned to the figures from before
/// `GROVE_V5` added the byte.
#[test]
fn test_average_case_merk_insert_element_wrapped_tree_is_unchanged_on_grove_v4() {
    let latest = GroveVersion::latest();
    // `added_bytes` of each wrapped tree of `wrapped_trees`, under a 4-byte
    // key in a count and sum tree.
    let pinned = [147, 151, 156, 165, 156, 167, 165, 156, 169];
    let cases = wrapped_trees();
    assert_eq!(
        cases.len(),
        pinned.len(),
        "every wrapped tree needs a pinned V4 figure",
    );
    for ((wrapped, bare), added_bytes) in cases.into_iter().zip(pinned) {
        let wrapped_cost = average_insert_element_cost(&wrapped, None, &GROVE_V4);
        assert_eq!(
            wrapped_cost.storage_cost.added_bytes, added_bytes,
            "{wrapped}: V4 estimate must not move",
        );
        assert_eq!(
            wrapped_cost,
            average_insert_element_cost(&bare, None, &GROVE_V4),
            "{wrapped}: V4 estimates the tree as it does without its wrapper",
        );
        assert_eq!(
            average_insert_element_cost(&bare, None, &GROVE_V4),
            average_insert_element_cost(&bare, None, latest),
            "{bare}: an unwrapped tree is estimated the same in V4 and V5",
        );
    }
}

/// The value hash is estimated at one call per 64-byte block of the sized
/// value. A plain tree with 60 flag bytes is sized at exactly 64 bytes (3 for
/// the tree, 60 flag bytes and 1 for their length), so from `GROVE_V5` the
/// byte of its wrapper takes it into a second block and adds one hash call.
/// One flag byte fewer leaves room for the wrapper's byte in the first block.
#[test]
fn test_average_case_merk_insert_element_wrapper_byte_can_add_a_value_hash_block() {
    let grove_version = GroveVersion::latest();
    let wrapped_and_bare = |flags_len: usize| {
        let bare = Element::empty_tree_with_flags(Some(vec![7; flags_len]));
        let wrapped = Element::new_non_counted(bare.clone()).expect("a tree wraps as non-counted");
        (wrapped, bare)
    };

    let (wrapped, bare) = wrapped_and_bare(60);
    let wrapped_cost = average_insert_element_cost(&wrapped, None, grove_version);
    let bare_cost = average_insert_element_cost(&bare, None, grove_version);
    assert_eq!(
        wrapped_cost.hash_node_calls,
        bare_cost.hash_node_calls + 1,
        "a 65-byte wrapped tree must be estimated one hash call above the 64-byte tree",
    );
    assert_eq!(
        wrapped_cost.storage_cost.added_bytes,
        bare_cost.storage_cost.added_bytes + 1,
    );
    assert_eq!(
        wrapped_cost,
        average_insert_tree_cost(&wrapped, 1, None, grove_version),
    );
    // `GROVE_V4` sizes the wrapped tree as the tree alone, hash calls
    // included.
    assert_eq!(
        average_insert_element_cost(&wrapped, None, &GROVE_V4),
        average_insert_element_cost(&bare, None, &GROVE_V4),
    );

    let (wrapped, bare) = wrapped_and_bare(59);
    assert_eq!(
        average_insert_element_cost(&wrapped, None, grove_version).hash_node_calls,
        average_insert_element_cost(&bare, None, grove_version).hash_node_calls,
        "a 64-byte wrapped tree still fits the first block",
    );
}
