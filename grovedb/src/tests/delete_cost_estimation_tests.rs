//! Tests for delete cost estimation functions in
//! `operations/delete/average_case.rs` and `operations/delete/worst_case.rs`.

use grovedb_merk::{
    estimated_costs::average_case_costs::{
        EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes,
    },
    tree_type::TreeType,
};
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;
use intmap::IntMap;

use crate::{
    batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
    GroveDb,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn normal_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

fn sum_tree_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::SumTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(50),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests for average_case_delete_operation_for_delete
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_average_case_delete_operation_for_delete_with_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        true, // validate
        true, // check_if_tree
        0,    // except_keys_count
        (8, 100),
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0,
        "validate + check_if_tree should produce seeks: {cost:?}"
    );
    assert!(cost.storage_loaded_bytes > 0, "should load bytes: {cost:?}");
}

#[test]
fn test_average_case_delete_operation_for_delete_no_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false, // validate
        false, // check_if_tree
        0,
        (8, 100),
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    // Even without validate/check_if_tree, is_empty_tree_except adds cost
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_loaded_bytes > 0 || cost.hash_node_calls > 0,
        "should produce cost from is_empty_tree_except: {cost:?}"
    );
}

#[test]
fn test_average_case_delete_operation_for_delete_sum_tree() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"sum_key".to_vec());

    let result = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::SumTree,
        true,
        true,
        0,
        (8, 100),
        grove_version,
    );

    result
        .value
        .as_ref()
        .expect("SumTree delete should succeed");
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests for average_case_delete_operations_for_delete_up_tree_while_empty
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_average_case_delete_up_tree_single_level() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let mut estimated_layer_info = IntMap::new();
    // height 0 == path_len - 1, so this is the leaf-level entry
    estimated_layer_info.insert(0u16, normal_layer_info());

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            estimated_layer_info,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(ops.len(), 1, "single-level path should produce 1 op");
}

#[test]
fn test_average_case_delete_up_tree_multi_level() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"a".to_vec()),
        KnownKey(b"b".to_vec()),
        KnownKey(b"c".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut estimated_layer_info = IntMap::new();
    // heights 0, 1, 2  (path_len = 3)
    estimated_layer_info.insert(0u16, normal_layer_info());
    estimated_layer_info.insert(1u16, normal_layer_info());
    estimated_layer_info.insert(2u16, normal_layer_info()); // leaf level

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            estimated_layer_info,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(ops.len(), 3, "3-level path should produce 3 ops");
}

#[test]
fn test_average_case_delete_up_tree_with_stop_height() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"a".to_vec()),
        KnownKey(b"b".to_vec()),
        KnownKey(b"c".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut estimated_layer_info = IntMap::new();
    // Only need layers for heights 1 and 2 (stop at 1)
    estimated_layer_info.insert(1u16, normal_layer_info());
    estimated_layer_info.insert(2u16, normal_layer_info());

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(1), // stop at height 1
            true,
            estimated_layer_info,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(
        ops.len(),
        2,
        "stop_path_height=1 with path_len=3 should produce 2 ops"
    );
}

#[test]
fn test_average_case_delete_up_tree_path_too_short_error() {
    let grove_version = GroveVersion::latest();
    // path len = 1, stop_path_height = 5 → error
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"short".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let estimated_layer_info = IntMap::new();

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(5),
            true,
            estimated_layer_info,
            grove_version,
        );

    assert!(result.value.is_err(), "should fail when path < stop height");
}

#[test]
fn test_average_case_delete_up_tree_missing_layer_info_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec()), KnownKey(b"b".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    // Provide layer info only for height 1 (leaf level), missing height 0
    let mut estimated_layer_info = IntMap::new();
    estimated_layer_info.insert(1u16, normal_layer_info());

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            estimated_layer_info,
            grove_version,
        );

    assert!(
        result.value.is_err(),
        "should fail when layer info is missing for a height"
    );
}

#[test]
fn test_average_case_delete_up_tree_with_sum_tree_layers() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"root".to_vec()),
        KnownKey(b"sum_branch".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut estimated_layer_info = IntMap::new();
    estimated_layer_info.insert(0u16, sum_tree_layer_info());
    estimated_layer_info.insert(1u16, sum_tree_layer_info()); // leaf level

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            estimated_layer_info,
            grove_version,
        );

    let ops = result.value.expect("sum tree layers should succeed");
    assert_eq!(ops.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests for worst_case_delete_operation_for_delete
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_worst_case_delete_operation_for_delete_with_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        true, // validate
        true, // check_if_tree
        0,    // except_keys_count
        256,  // max_element_size
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0,
        "validate + check_if_tree should produce seeks: {cost:?}"
    );
    assert!(cost.storage_loaded_bytes > 0, "should load bytes: {cost:?}");
}

#[test]
fn test_worst_case_delete_operation_for_delete_no_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false, // validate
        false, // check_if_tree
        0,
        256,
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_loaded_bytes > 0 || cost.hash_node_calls > 0,
        "should produce cost from is_empty_tree_except: {cost:?}"
    );
}

#[test]
fn test_worst_case_delete_operation_for_delete_sum_tree() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"sum_key".to_vec());

    let result = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::SumTree,
        true,
        true,
        0,
        256,
        grove_version,
    );

    result
        .value
        .as_ref()
        .expect("SumTree delete should succeed");
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests for worst_case_delete_operations_for_delete_up_tree_while_empty
// ═══════════════════════════════════════════════════════════════════════════════

// Note: In worst_case.rs the loop condition `if height == path_len` is never
// true (the range is `stop_path_height..path_len`), so the else branch always
// executes.  intermediate_tree_info is looked up for every height.

#[test]
fn test_worst_case_delete_up_tree_single_level() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let mut intermediate_tree_info = IntMap::new();
    intermediate_tree_info.insert(0u64, (TreeType::NormalTree, 0u32));

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(ops.len(), 1, "single-level path should produce 1 op");
}

#[test]
fn test_worst_case_delete_up_tree_multi_level() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"a".to_vec()),
        KnownKey(b"b".to_vec()),
        KnownKey(b"c".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut intermediate_tree_info = IntMap::new();
    intermediate_tree_info.insert(0u64, (TreeType::NormalTree, 0u32));
    intermediate_tree_info.insert(1u64, (TreeType::NormalTree, 0u32));
    intermediate_tree_info.insert(2u64, (TreeType::NormalTree, 0u32));

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(ops.len(), 3, "3-level path should produce 3 ops");
}

#[test]
fn test_worst_case_delete_up_tree_with_stop_height() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"a".to_vec()),
        KnownKey(b"b".to_vec()),
        KnownKey(b"c".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut intermediate_tree_info = IntMap::new();
    intermediate_tree_info.insert(1u64, (TreeType::NormalTree, 0u32));
    intermediate_tree_info.insert(2u64, (TreeType::NormalTree, 0u32));

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(1),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    let ops = result.value.expect("should return ops");
    assert_eq!(
        ops.len(),
        2,
        "stop_path_height=1 with path_len=3 should produce 2 ops"
    );
}

#[test]
fn test_worst_case_delete_up_tree_path_too_short_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"short".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let intermediate_tree_info = IntMap::new();

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(5),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    assert!(result.value.is_err(), "should fail when path < stop height");
}

#[test]
fn test_worst_case_delete_up_tree_missing_tree_info_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec()), KnownKey(b"b".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    // Provide info for height 1 but not height 0
    let mut intermediate_tree_info = IntMap::new();
    intermediate_tree_info.insert(1u64, (TreeType::NormalTree, 0u32));

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    assert!(
        result.value.is_err(),
        "should fail when intermediate tree info is missing"
    );
}

#[test]
fn test_worst_case_delete_up_tree_with_sum_tree_layers() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![
        KnownKey(b"root".to_vec()),
        KnownKey(b"sum_branch".to_vec()),
    ]);
    let key = KnownKey(b"leaf".to_vec());

    let mut intermediate_tree_info = IntMap::new();
    intermediate_tree_info.insert(0u64, (TreeType::SumTree, 4u32));
    intermediate_tree_info.insert(1u64, (TreeType::SumTree, 4u32));

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            intermediate_tree_info,
            256,
            grove_version,
        );

    let ops = result.value.expect("sum tree layers should succeed");
    assert_eq!(ops.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Comparison: worst case ≥ average case
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_worst_case_costs_gte_average_case_for_single_delete() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let avg = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        true,
        true,
        0,
        (8, 100),
        grove_version,
    );

    let worst = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        true,
        true,
        0,
        256,
        grove_version,
    );

    avg.value.as_ref().expect("avg should succeed");
    worst.value.as_ref().expect("worst should succeed");

    assert!(
        worst.cost.seek_count >= avg.cost.seek_count,
        "worst-case seeks ({}) should be >= average-case seeks ({})",
        worst.cost.seek_count,
        avg.cost.seek_count
    );
    assert!(
        worst.cost.storage_loaded_bytes >= avg.cost.storage_loaded_bytes,
        "worst-case loaded bytes ({}) should be >= average-case loaded bytes ({})",
        worst.cost.storage_loaded_bytes,
        avg.cost.storage_loaded_bytes
    );
}
