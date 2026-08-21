//! Shipped compaction hash count.
//!
//! The chunk-blob leaf hash plus one per peak the MMR push collapses. It omits
//! the peak bagging the compaction's own `get_root` performs, so a compaction
//! landing on a multi-peak MMR under-reports by `peaks - 1`.
//!
//! Locked: GROVE_V1..V3 are released and CommitmentTree has been billing this
//! figure on mainnet.

use grovedb_merkle_mountain_range::hash_count_for_push;

pub(super) fn compaction_hash_count(leaf_count: u64) -> u32 {
    hash_count_for_push(leaf_count)
}
