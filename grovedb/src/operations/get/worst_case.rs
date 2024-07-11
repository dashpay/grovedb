//! Worst case get costs

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
    /// Worst case cost for has raw
    pub fn worst_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "worst_case_for_has_raw",
            grove_version
                .grovedb_versions
                .operations
                .get
                .worst_case_for_has_raw
        );
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Worst case cost for get raw
    pub fn worst_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "worst_case_for_get_raw",
            grove_version
                .grovedb_versions
                .operations
                .get
                .worst_case_for_get_raw
        );
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
            grove_version,
        )?;
        Ok(cost)
    }

    /// Worst case cost for get
    pub fn worst_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
        in_parent_tree_using_sums: bool,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        check_grovedb_v0!(
            "worst_case_for_get",
            grove_version
                .grovedb_versions
                .operations
                .get
                .worst_case_for_get
        );
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
            max_references_sizes,
            grove_version,
        )?;
        Ok(cost)
    }
}
