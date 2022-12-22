use costs::OperationCost;
use storage::rocksdb_storage::RocksDbStorage;
use crate::batch::key_info::KeyInfo;
use crate::batch::KeyInfoPath;
use crate::GroveDb;

#[cfg(feature = "full")]
impl GroveDb {
    /// Get the Operation Cost for a has query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a has query where we estimate that we
    /// would get a tree with the following parameters
    pub fn average_case_for_has_raw_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a get query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_using_sums: bool,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            in_parent_tree_using_sums,
            estimated_element_size,
            estimated_references_sizes,
        );
        cost
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
        );
        cost
    }
}
