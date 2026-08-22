//! Corrected compaction hash count.
//!
//! Adds the peak-bagging merges [`v0`](super::v0) omitted. Used from GROVE_V4.

#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::LeafValueStorageCost;
use grovedb_merkle_mountain_range::{hash_count_for_push, hash_count_for_root_bagging};

#[cfg(feature = "storage")]
use super::{AppendStorageAccounting, SlotRewriteAccounting};
#[cfg(feature = "storage")]
use crate::chunk::chunk_blob_entry_bytes;

pub(super) fn compaction_hash_count(leaf_count: u64, mmr_size_after_push: u64) -> u32 {
    // Derived from the MMR shape rather than read back out of the accumulated
    // `OperationCost`, so this stays correct regardless of whether
    // `MMR::get_root`'s own charge is enabled for the caller's version — the
    // two gates are independent.
    hash_count_for_push(leaf_count).saturating_add(hash_count_for_root_bagging(mmr_size_after_push))
}

/// Churn-as-replacement storage accounting (issue #822). Used from GROVE_V4.
///
/// - The entry's chunk-blob share (its own bytes) is charged as added
///   storage at its append — these are the bytes that persist.
/// - The buffer slot write is sized against the value the slot already holds
///   in committed storage, which is read first (and the read billed): a
///   rewrite (epoch 2 onward) is replaced, growth is added, shrink is not
///   credited; a slot written for the first time stays fully added and is
///   not read.
/// - The compaction blob is reported as a replacement of the entry bytes it
///   supersedes (all prepaid), leaving only its framing — and the MMR
///   internal nodes — as added storage.
#[cfg(feature = "storage")]
pub(super) fn append_storage_accounting() -> AppendStorageAccounting {
    AppendStorageAccounting {
        slot_rewrite: SlotRewriteAccounting::AgainstCommitted,
        chunk_leaf: LeafValueStorageCost::PartlyPrepaid(chunk_blob_entry_bytes),
        prepay_chunk_share: true,
    }
}
