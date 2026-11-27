//! `add_worst_case_get_merk_at_path` **v1** (`GROVE_V6`).
//!
//! A write opens a Merk by its own path and root. Differs from [`super::v0`]
//! in that the read of the host is gone: one seek and no loaded bytes,
//! where v0 has two seeks and the bytes of that read. The root Merk has no
//! host and keeps its two seeks: the read of its root key record and the
//! fetch of its root. This version has the bytes of that record too, which
//! v0 does not: the 256 the worst case takes a key to be. The open is added
//! to the cost it is given in full, or refused with nothing added, where v0
//! adds its seeks unchecked. See the module docs in [`super`][`mod@super`].

use grovedb_costs::OperationCost;
use grovedb_merk::{
    cost_model::storage_cost_model, estimated_costs::worst_case_costs::MERK_BIGGEST_KEY_SIZE,
};
use grovedb_storage::worst_case_costs::get_storage_context_cost;
use grovedb_version::version::GroveVersion;

use crate::{batch::KeyInfoPath, estimated_costs::add_in_full, Error, GroveDb};

impl GroveDb {
    /// `add_worst_case_get_merk_at_path` v1: see the module documentation.
    pub(super) fn add_worst_case_get_merk_at_path_v1(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        let is_the_root_merk = path.last().is_none();
        // The open by itself: the prefix of the path, the fetch of the
        // root, and for the root Merk the read of its root key record.
        let mut open = get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        open.seek_count += 1 + u32::from(is_the_root_merk);
        if is_the_root_merk {
            open.storage_loaded_bytes += u64::from(MERK_BIGGEST_KEY_SIZE);
        }
        // Added to what the caller has in full, or refused with nothing
        // added.
        add_in_full(cost, open)
    }
}
