//! `add_average_case_get_merk_at_path` **v1** (`GROVE_V6`).
//!
//! A write opens a Merk by its own path and root. Differs from [`super::v0`]
//! in that the read of the host is gone: no seek and no loaded bytes for
//! it. The root Merk has no host: it is opened by its root key record, and
//! this version has the bytes of that record with its seek, which v0 does
//! not. They are the root key, at the length the caller gives or, given
//! none, at the 256 bytes the worst case takes a key to be. See the module
//! docs in [`super`][`mod@super`].

use grovedb_costs::OperationCost;
use grovedb_merk::{
    cost_model::storage_cost_model, estimated_costs::worst_case_costs::MERK_BIGGEST_KEY_SIZE,
};
use grovedb_storage::worst_case_costs::get_storage_context_cost;
use grovedb_version::version::GroveVersion;

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// `add_average_case_get_merk_at_path` v1: see the module documentation.
    pub(super) fn add_average_case_get_merk_at_path_v1(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        merk_should_be_empty: bool,
        root_key_len: Option<u32>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        let is_the_root_merk = path.last().is_none();
        // A root Merk with no node has no root key in its record.
        let record = if is_the_root_merk && !merk_should_be_empty {
            u64::from(root_key_len.unwrap_or(MERK_BIGGEST_KEY_SIZE))
        } else {
            0
        };
        // Before anything is added: nothing is where this is refused.
        let loaded = cost
            .storage_loaded_bytes
            .checked_add(record)
            .ok_or(Error::Overflow(
                "overflow for the bytes of the root key record",
            ))?;
        if is_the_root_merk {
            cost.seek_count += 1;
        }
        // If the merk is not empty we load the tree
        if !merk_should_be_empty {
            cost.seek_count += 1;
        }
        cost.storage_loaded_bytes = loaded;
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());

        Ok(())
    }
}
