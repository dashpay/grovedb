//! Churn-as-replacement frontier-save accounting (issue #822). Used from
//! GROVE_V4.
//!
//! The frontier is one value rewritten in place on every append. When a
//! committed frontier was loaded at open, the rewrite is reported as a
//! replacement of it — `replaced_bytes` is the smaller of the previous and
//! the new paid size, `added_bytes` is growth only, and shrink (a position
//! with fewer ommers) is not credited — and the key, which exists, is not
//! charged. The very first save of a tree creates the key and stays fully
//! added.

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;

pub(super) fn frontier_save_cost_info(
    persisted_len: Option<u32>,
    new_len: u32,
) -> Option<KeyValueStorageCost> {
    // The same "replace what was there, add the growth" shape the
    // bulk-append tree uses for a rewritten buffer slot.
    persisted_len.map(|previous| KeyValueStorageCost::for_in_place_value_rewrite(previous, new_len))
}
