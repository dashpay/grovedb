//! The fifteen estimate helpers that take a storage type parameter give what
//! the engine-free helpers of the same name give.

use grovedb_costs::OperationCost;
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
    batch::{
        key_info::KeyInfo::{self, KnownKey},
        KeyInfoPath,
    },
    BackwardsReferences, Error, GroveDb,
};

fn path() -> KeyInfoPath {
    KeyInfoPath::from_vec(vec![
        KnownKey(b"root".to_vec()),
        KnownKey(b"child".to_vec()),
    ])
}

fn key() -> KeyInfo {
    KnownKey(b"key".to_vec())
}

fn added(add: impl FnOnce(&mut OperationCost) -> Result<(), Error>) -> OperationCost {
    let mut cost = OperationCost::default();
    add(&mut cost).expect("the estimate succeeds");
    assert_ne!(cost, OperationCost::default());
    cost
}

#[test]
fn a_worst_case_helper_with_a_storage_parameter_is_the_engine_free_one() {
    let v = GroveVersion::latest();
    let (path, key) = (path(), key());
    let tree = TreeType::NormalTree;

    assert_eq!(
        added(
            |cost| GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(cost, &path, tree, v)
        ),
        added(|cost| GroveDb::add_worst_case_get_merk_at_path_engine_free(cost, &path, tree, v)),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
                cost, &path, &key, 100, tree, v
            )
        ),
        added(|cost| GroveDb::add_worst_case_has_raw_cost_engine_free(
            cost, &path, &key, 100, tree, v
        )),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_worst_case_get_raw_tree_cost::<RocksDbStorage>(
                cost,
                &path,
                &key,
                TreeType::SumTree,
                tree,
                v
            )
        ),
        added(
            |cost| GroveDb::add_worst_case_get_raw_tree_cost_engine_free(
                cost,
                &path,
                &key,
                TreeType::SumTree,
                tree,
                v
            )
        ),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
                cost, &path, &key, 100, tree, v
            )
        ),
        added(|cost| GroveDb::add_worst_case_get_raw_cost_engine_free(
            cost, &path, &key, 100, tree, v
        )),
    );
    assert_eq!(
        added(|cost| GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
            cost,
            &path,
            &key,
            100,
            tree,
            vec![40, 50],
            v
        )),
        added(|cost| GroveDb::add_worst_case_get_cost_engine_free(
            cost,
            &path,
            &key,
            100,
            tree,
            vec![40, 50],
            v
        )),
    );
}

#[test]
fn an_average_case_helper_with_a_storage_parameter_is_the_engine_free_one() {
    let v = GroveVersion::latest();
    let (path, key) = (path(), key());
    let tree = TreeType::NormalTree;

    assert_eq!(
        added(
            |cost| GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                cost, &path, false, tree, v
            )
        ),
        added(
            |cost| GroveDb::add_average_case_get_merk_at_path_engine_free(
                cost, &path, false, tree, v
            )
        ),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
                cost, &path, &key, 100, tree, v
            )
        ),
        added(|cost| GroveDb::add_average_case_has_raw_cost_engine_free(
            cost, &path, &key, 100, tree, v
        )),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
                cost,
                &path,
                &key,
                4,
                TreeType::SumTree,
                tree,
                v
            )
        ),
        added(
            |cost| GroveDb::add_average_case_has_raw_tree_cost_engine_free(
                cost,
                &path,
                &key,
                4,
                TreeType::SumTree,
                tree,
                v
            )
        ),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
                cost, &path, &key, 100, tree, v
            )
        ),
        added(|cost| GroveDb::add_average_case_get_raw_cost_engine_free(
            cost, &path, &key, 100, tree, v
        )),
    );
    assert_eq!(
        added(
            |cost| GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
                cost,
                &path,
                &key,
                4,
                TreeType::SumTree,
                tree,
                v
            )
        ),
        added(
            |cost| GroveDb::add_average_case_get_raw_tree_cost_engine_free(
                cost,
                &path,
                &key,
                4,
                TreeType::SumTree,
                tree,
                v
            )
        ),
    );
    assert_eq!(
        added(|cost| GroveDb::add_average_case_get_cost::<RocksDbStorage>(
            cost,
            &path,
            &key,
            tree,
            100,
            vec![40, 50],
            v
        )),
        added(|cost| GroveDb::add_average_case_get_cost_engine_free(
            cost,
            &path,
            &key,
            tree,
            100,
            vec![40, 50],
            v
        )),
    );
}

#[test]
fn a_delete_estimate_with_a_storage_parameter_is_the_engine_free_one() {
    let v = GroveVersion::latest();
    let (path, key) = (path(), key());
    let tree = TreeType::NormalTree;
    let check = BackwardsReferences::Check;

    let with_storage = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path, &key, tree, true, true, 0, 100, check, v,
    );
    let engine_free = GroveDb::worst_case_delete_operation_for_delete_engine_free(
        &path, &key, tree, true, true, 0, 100, check, v,
    );
    assert_ne!(engine_free.cost, OperationCost::default());
    assert_eq!(with_storage.cost, engine_free.cost);
    assert_eq!(with_storage.value.unwrap(), engine_free.value.unwrap());

    let with_storage = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        tree,
        true,
        true,
        0,
        (8, 100),
        check,
        v,
    );
    let engine_free = GroveDb::average_case_delete_operation_for_delete_engine_free(
        &path,
        &key,
        tree,
        true,
        true,
        0,
        (8, 100),
        check,
        v,
    );
    assert_ne!(engine_free.cost, OperationCost::default());
    assert_eq!(with_storage.cost, engine_free.cost);
    assert_eq!(with_storage.value.unwrap(), engine_free.value.unwrap());

    let worst_layers = || {
        let mut layers = IntMap::new();
        layers.insert(0u64, (tree, 100u32));
        layers.insert(1u64, (tree, 100u32));
        layers
    };
    let with_storage = GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty::<
        RocksDbStorage,
    >(&path, &key, Some(0), true, worst_layers(), 100, check, v);
    let engine_free =
        GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty_engine_free(
            &path,
            &key,
            Some(0),
            true,
            worst_layers(),
            100,
            check,
            v,
        );
    assert_ne!(engine_free.cost, OperationCost::default());
    assert_eq!(with_storage.cost, engine_free.cost);
    let (with_storage, engine_free) = (with_storage.value.unwrap(), engine_free.value.unwrap());
    assert!(!engine_free.is_empty());
    assert_eq!(with_storage, engine_free);

    let average_layers = || {
        let layer = || EstimatedLayerInformation {
            tree_type: tree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(8, Default::default(), None),
        };
        let mut layers = IntMap::new();
        layers.insert(0u16, layer());
        layers.insert(1u16, layer());
        layers.insert(2u16, layer());
        layers
    };
    let with_storage = GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty::<
        RocksDbStorage,
    >(&path, &key, Some(0), true, average_layers(), check, v);
    let engine_free =
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty_engine_free(
            &path,
            &key,
            Some(0),
            true,
            average_layers(),
            check,
            v,
        );
    assert_ne!(engine_free.cost, OperationCost::default());
    assert_eq!(with_storage.cost, engine_free.cost);
    let (with_storage, engine_free) = (with_storage.value.unwrap(), engine_free.value.unwrap());
    assert!(!engine_free.is_empty());
    assert_eq!(with_storage, engine_free);
}
