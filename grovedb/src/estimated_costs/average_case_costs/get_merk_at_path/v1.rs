//! `add_average_case_get_merk_at_path` **v1** (`GROVE_V6`).
//!
//! A write opens a Merk by its own path and root. Differs from [`super::v0`]
//! in that the read of the host is gone: no seek and no loaded bytes for
//! it. The root Merk has no host: it is opened by its root key record, and
//! this version has the bytes of that record with its seek, which v0 does
//! not. They are the root key, at the length the caller gives or, given
//! none, at the 256 bytes the worst case takes a key to be. The open is
//! added to the cost it is given in full, or refused with nothing added,
//! where v0 adds its seeks unchecked. See the module docs in
//! [`super`][`mod@super`].

use grovedb_costs::OperationCost;
use grovedb_merk::{
    cost_model::storage_cost_model, estimated_costs::worst_case_costs::MERK_BIGGEST_KEY_SIZE,
};
use grovedb_storage::worst_case_costs::get_storage_context_cost;
use grovedb_version::version::GroveVersion;

use crate::{batch::KeyInfoPath, estimated_costs::add_in_full, Error, GroveDb};

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
        // The open by itself: the prefix of the path, the read of the root
        // key record of the root Merk, and the fetch of the root of a Merk
        // that is not expected to be empty. A root Merk with no node has no
        // root key in its record.
        let mut open = get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        open.seek_count += u32::from(is_the_root_merk) + u32::from(!merk_should_be_empty);
        if is_the_root_merk && !merk_should_be_empty {
            open.storage_loaded_bytes += u64::from(root_key_len.unwrap_or(MERK_BIGGEST_KEY_SIZE));
        }
        // Added to what the caller has in full, or refused with nothing
        // added.
        add_in_full(cost, open)
    }
}
