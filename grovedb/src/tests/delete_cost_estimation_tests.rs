//! Tests for delete cost estimation functions in
//! `operations/delete/average_case.rs` and `operations/delete/worst_case.rs`.
//!
//! Each test targets a unique branch; see inline comments for line coverage.

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
    batch::{key_info::KeyInfo::KnownKey, GroveOp, KeyInfoPath},
    GroveDb,
};

fn normal_layer_info() -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
        estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  average_case_delete_operation_for_delete  (average_case.rs lines 138–196)
// ═══════════════════════════════════════════════════════════════════════════════

/// Unique coverage: validate=false (skips lines 158-169),
/// check_if_tree=false (skips lines 170-182).
/// The validate=true + check_if_tree=true/false branches are already covered
/// by the multi_level up-tree test below (leaf and intermediate iterations).
#[test]
fn test_average_case_delete_no_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false,
        false,
        0,
        (8, 100),
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_loaded_bytes > 0,
        "is_empty_tree_except should still add cost: {cost:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
//  average_case_delete_operations_for_delete_up_tree_while_empty
//  (average_case.rs lines 29–135)
// ═══════════════════════════════════════════════════════════════════════════════

/// Covers the happy path: leaf branch 3a (line 70-87) via height==path_len-1,
/// intermediate branch 4a (lines 93-112) via lower heights, and indirectly
/// exercises average_case_delete_operation_for_delete with validate=true +
/// check_if_tree=true (leaf) and check_if_tree=false (intermediate).
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
    estimated_layer_info.insert(0u16, normal_layer_info());
    estimated_layer_info.insert(1u16, normal_layer_info());
    estimated_layer_info.insert(2u16, normal_layer_info());

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

    // The leaf iteration (height == path_len - 1) uses the provided key.
    let first = &ops[0];
    assert_eq!(first.op, GroveOp::Delete);
    assert_eq!(first.key, Some(KnownKey(b"leaf".to_vec())));
}

/// Covers error branch 1 (lines 49-54): path.len() < stop_path_height.
#[test]
fn test_average_case_delete_up_tree_path_too_short_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"short".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let result = GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<
        RocksDbStorage,
    >(&path, &key, Some(5), true, IntMap::new(), grove_version);

    assert!(result.value.is_err(), "should fail when path < stop height");
}

/// Covers branch 4b (line 113-115): intermediate layer info missing.
/// path_len=2, layer info provided only for leaf height (1), missing height 0.
#[test]
fn test_average_case_delete_up_tree_missing_intermediate_info_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec()), KnownKey(b"b".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let mut estimated_layer_info = IntMap::new();
    estimated_layer_info.insert(1u16, normal_layer_info());
    // height 0 deliberately missing

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
        "should fail when intermediate layer info is missing"
    );
}

/// Covers branch 3b (lines 88-92): leaf-level layer info missing.
/// path_len=1 so the only iteration is the leaf branch; empty IntMap triggers
/// the "intermediate flag size missing for height at path length" error.
#[test]
fn test_average_case_delete_up_tree_missing_leaf_info_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let result =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(0),
            true,
            IntMap::new(), // no layer info at all
            grove_version,
        );

    assert!(
        result.value.is_err(),
        "should fail when leaf-level layer info is missing"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
//  worst_case_delete_operation_for_delete  (worst_case.rs lines 113–166)
// ═══════════════════════════════════════════════════════════════════════════════

/// Covers validate=true (lines 133-143) and check_if_tree=true (lines 144-156).
/// The up-tree multi_level test below only ever passes check_if_tree=false
/// (because `if height == path_len` is dead code), so this test is the sole
/// coverage for lines 144-156.
#[test]
fn test_worst_case_delete_with_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        true,
        true,
        0,
        256,
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0,
        "validate + check_if_tree should produce seeks: {cost:?}"
    );
}

/// Covers validate=false (skips lines 133-143).
/// Multi_level always passes validate=true, so this is the sole coverage for
/// the false branch.
#[test]
fn test_worst_case_delete_no_validate() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"root".to_vec())]);
    let key = KnownKey(b"item_key".to_vec());

    let result = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false,
        false,
        0,
        256,
        grove_version,
    );

    result.value.as_ref().expect("should succeed");
    let cost = result.cost;
    assert!(
        cost.seek_count > 0 || cost.storage_loaded_bytes > 0,
        "is_empty_tree_except should still add cost: {cost:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
//  worst_case_delete_operations_for_delete_up_tree_while_empty
//  (worst_case.rs lines 23–110)
// ═══════════════════════════════════════════════════════════════════════════════

/// Covers the happy-path else branch 4a (lines 72-107).
/// Note: `if height == path_len` (line 64) is dead code — the loop range
/// `stop_path_height..path_len` is exclusive, so the else branch always runs.
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

    // Because `if height == path_len` (line 64) is dead code, the else branch
    // always runs and uses the last path segment as the key — NOT the provided
    // `key` ("leaf"). The first iteration pops "c" from the path.
    let first = &ops[0];
    assert_eq!(first.op, GroveOp::Delete);
    assert_eq!(first.key, Some(KnownKey(b"c".to_vec())));
}

/// Covers error branch 1 (lines 44-49): path.len() < stop_path_height.
#[test]
fn test_worst_case_delete_up_tree_path_too_short_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"short".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

    let result =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<RocksDbStorage>(
            &path,
            &key,
            Some(5),
            true,
            IntMap::new(),
            256,
            grove_version,
        );

    assert!(result.value.is_err(), "should fail when path < stop height");
}

/// Covers branch 4b (line 88-90): intermediate tree info missing.
/// path_len=2, info provided only for height 1, missing height 0.
#[test]
fn test_worst_case_delete_up_tree_missing_tree_info_error() {
    let grove_version = GroveVersion::latest();
    let path = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec()), KnownKey(b"b".to_vec())]);
    let key = KnownKey(b"leaf".to_vec());

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
