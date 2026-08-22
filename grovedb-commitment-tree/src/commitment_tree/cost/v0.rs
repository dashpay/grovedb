//! Shipped frontier-save accounting: no cost information, so the commit path
//! bills the key and the whole serialized frontier as new storage on every
//! append.
//!
//! Locked: GROVE_V1..V3 are released and the shielded pool has been billed
//! this way on mainnet.

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;

pub(super) fn frontier_save_cost_info() -> Option<KeyValueStorageCost> {
    None
}
