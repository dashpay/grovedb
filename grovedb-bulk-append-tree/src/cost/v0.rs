//! Shipped compaction hash count.
//!
//! The chunk-blob leaf hash plus one per peak the MMR push collapses. It omits
//! the peak bagging the compaction's own `get_root` performs, so a compaction
//! landing on a multi-peak MMR under-reports by `peaks - 1`.
//!
//! Locked: GROVE_V1..V3 are released and CommitmentTree has been billing this
//! figure on mainnet.

#[cfg(feature = "storage")]
use grovedb_dense_fixed_sized_merkle_tree::SlotWriteAccounting;
use grovedb_merkle_mountain_range::hash_count_for_push;
#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::LeafValueStorageCost;

#[cfg(feature = "storage")]
use super::AppendStorageAccounting;

pub(super) fn compaction_hash_count(leaf_count: u64) -> u32 {
    hash_count_for_push(leaf_count)
}

/// Shipped storage accounting: every data put is issued with no cost
/// information, so the commit path bills key + value as new storage — the
/// slot rewrite, the chunk blob and the MMR nodes alike — and nothing is
/// charged at append time beyond the slot put itself.
///
/// Locked: GROVE_V1..V3 are released and the shielded pool has been billed
/// this way on mainnet.
#[cfg(feature = "storage")]
pub(super) fn append_storage_accounting() -> AppendStorageAccounting {
    AppendStorageAccounting {
        slot_write: SlotWriteAccounting::AsNew,
        chunk_leaf: LeafValueStorageCost::New,
        prepay_chunk_share: false,
    }
}
