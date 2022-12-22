#[cfg(feature = "full")]
use costs::OperationCost;
#[cfg(feature = "full")]
use storage::rocksdb_storage::RocksDbStorage;

#[cfg(feature = "full")]
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    GroveDb,
};

#[cfg(feature = "full")]
impl GroveDb {
    pub fn worst_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    pub fn worst_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    pub fn worst_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
            max_references_sizes,
        );
        cost
    }
}
