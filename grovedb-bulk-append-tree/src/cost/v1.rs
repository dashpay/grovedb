//! Fixed-model accounting. Used from GROVE_V4.
//!
//! The compaction's hashes are amortized into every append
//! (`AMORTIZED_COMPACTION_HASHES`), so a compacting append reports nothing
//! extra here; and each append's storage writes are charged as its long-term
//! footprint plus churn (issue #822).

#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::LeafValueStorageCost;

#[cfg(feature = "storage")]
use super::{AppendStorageAccounting, SlotRewriteAccounting};

/// A compacting append reports no hashes of its own: the chunk-leaf hash, the
/// MMR merges and the root bagging are amortized over the epoch as one blake3
/// per append (`AMORTIZED_COMPACTION_HASHES`), charged on every append.
pub(super) fn compaction_hash_count(_leaf_count: u64, _mmr_size_after_push: u64) -> u32 {
    0
}

/// Long-term-bytes storage accounting (issue #822). Used from GROVE_V4.
///
/// - The entry's chunk-blob share (its own bytes) is charged as added
///   storage at its append — these are the bytes that persist — together
///   with its share of the blob framing and MMR nodes, amortized over the
///   epoch.
/// - The dense buffer is churn: every slot write and every path record write
///   is reported as an in-place replacement of its own size — epoch 1
///   included, nothing added, no key charged, nothing read to size it. The
///   buffer is a fixed-size per-tree scratch area rewritten every epoch, not
///   any entry's long-term storage.
/// - The compaction blob and the MMR nodes are prepaid: their puts carry
///   zero-byte cost information. Each append is charged its own bytes once
///   more as `replaced` — its part of the blob rewrite — so the epoch's blob
///   write is spread over the epoch's appends.
/// - Every append — buffered or compacting — is charged the buffer's fixed
///   root-maintenance model for its height plus one amortized compaction
///   blake3, whatever its position.
#[cfg(feature = "storage")]
pub(super) fn append_storage_accounting() -> AppendStorageAccounting {
    AppendStorageAccounting {
        slot_rewrite: SlotRewriteAccounting::Churn,
        chunk_leaf: LeafValueStorageCost::Prepaid,
        prepay_chunk_share: true,
        fixed_model: true,
    }
}
