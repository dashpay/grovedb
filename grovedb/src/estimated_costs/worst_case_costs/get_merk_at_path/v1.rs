//! `add_worst_case_get_merk_at_path` **v1** (`GROVE_V6`).
//!
//! A write opens a Merk by its own path and root. Differs from [`super::v0`]
//! only in that the read of the host is gone: one seek and no loaded bytes,
//! where v0 has two seeks and the bytes of that read. The root Merk has no
//! host and keeps its two seeks: the read of its root key record and the
//! fetch of its root. See the module docs in [`super`][`mod@super`].

use grovedb_costs::OperationCost;
use grovedb_merk::cost_model::storage_cost_model;
use grovedb_storage::worst_case_costs::get_storage_context_cost;
use grovedb_version::version::GroveVersion;

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// `add_worst_case_get_merk_at_path` v1: see the module documentation.
    pub(super) fn add_worst_case_get_merk_at_path_v1(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        cost.seek_count += if path.last().is_none() { 2 } else { 1 };
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        Ok(())
    }
}
