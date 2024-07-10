//! Exists
//! Implements in Element functions for checking if stuff exists

use grovedb_costs::CostResult;
use grovedb_merk::Merk;
use grovedb_storage::StorageContext;
use grovedb_version::check_v0_with_cost;
use grovedb_version::version::GroveVersion;
use crate::{Element, Error};
use grovedb_costs::OperationCost;
use grovedb_version::error::GroveVersionError;
use grovedb_costs::CostsExt;

impl Element {
    
    /// Helper function that returns whether an element at the key for the
    /// element already exists.
    pub fn element_at_key_already_exists<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_v0_with_cost!(
            "element_at_key_already_exists",
            grove_version
                .grovedb_versions
                .element
                .element_at_key_already_exists
        );
        merk.exists(
            key.as_ref(),
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }
}
