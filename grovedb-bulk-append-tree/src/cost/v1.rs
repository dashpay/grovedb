//! Corrected compaction hash count.
//!
//! Adds the peak-bagging merges [`v0`](super::v0) omitted. Used from GROVE_V4.

use grovedb_merkle_mountain_range::{hash_count_for_push, hash_count_for_root_bagging};

pub(super) fn compaction_hash_count(leaf_count: u64, mmr_size_after_push: u64) -> u32 {
    // Derived from the MMR shape rather than read back out of the accumulated
    // `OperationCost`, so this stays correct regardless of whether
    // `MMR::get_root`'s own charge is enabled for the caller's version — the
    // two gates are independent.
    hash_count_for_push(leaf_count).saturating_add(hash_count_for_root_bagging(mmr_size_after_push))
}
