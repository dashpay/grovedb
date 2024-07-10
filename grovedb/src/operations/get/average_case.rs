//! Average case get costs

#[cfg(feature = "full")]
use grovedb_costs::OperationCost;
#[cfg(feature = "full")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::{check_grovedb_v0, error::GroveVersionError, version::GroveVersion};

use crate::Error;
#[cfg(feature = "full")]
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    GroveDb,
};

#[cfg(feature = "full")]
impl GroveDb {
    /// Get the Operation Cost for a has query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "average_case_for_has_raw",
            grove_version
                .grovedb_versions
                .operations
                .get
                .average_case_for_has_raw
        );
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Get the Operation Cost for a has query where we estimate that we
    /// would get a tree with the following parameters
    pub fn average_case_for_has_raw_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "average_case_for_has_raw_tree",
            grove_version
                .grovedb_versions
                .operations
                .get
                .average_case_for_has_raw_tree
        );
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Get the Operation Cost for a get query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "average_case_for_get_raw",
            grove_version
                .grovedb_versions
                .operations
                .get
                .average_case_for_get_raw
        );
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_using_sums: bool,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "average_case_for_get",
            grove_version
                .grovedb_versions
                .operations
                .get
                .average_case_for_get
        );
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            in_parent_tree_using_sums,
            estimated_element_size,
            estimated_references_sizes,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "average_case_for_get",
            grove_version
                .grovedb_versions
                .operations
                .get
                .average_case_for_get_tree
        );
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }
}
