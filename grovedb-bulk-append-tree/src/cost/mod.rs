//! Versioned cost accounting for the bulk-append tree.
//!
//! Only cost reporting is versioned; chunk bytes, roots and stored state are
//! identical under every version. These gates reach live fees —
//! `CommitmentTree` adds the compaction hash count straight into its own
//! `hash_node_calls`, and every data put's cost information becomes storage
//! fees at commit — so corrected figures arrive as new versions rather than
//! replacing the old ones.
//!
//! Two gates live here:
//!
//! - `compaction_hash_count` — the hashes a compacting append reports.
//! - `append_storage_accounting` — how an append's data-storage writes are
//!   reported to the storage cost layer (issue #822). v0 issues every put
//!   with no cost information, so each is charged as new storage: a buffer
//!   slot rewritten in epoch 2+, and the chunk blob that supersedes the
//!   buffer, are both billed as permanent growth. v1 charges each entry's
//!   permanent bytes once — its chunk-blob share, at its own append — and
//!   reports the slot rewrite and the compaction blob as replacements of
//!   the bytes they supersede.

mod v0;
mod v1;

#[cfg(feature = "storage")]
use grovedb_dense_fixed_sized_merkle_tree::SlotWriteAccounting;
#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::LeafValueStorageCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::BulkAppendError;

/// How an append's data-storage writes are reported to the storage cost
/// layer. Selected per grove version by [`append_storage_accounting`].
#[cfg(feature = "storage")]
#[derive(Clone, Copy)]
pub(crate) struct AppendStorageAccounting {
    /// How the dense-buffer slot write is reported.
    pub slot_write: SlotWriteAccounting,
    /// How the chunk-blob leaf is reported when the MMR overlay is flushed.
    pub chunk_leaf: LeafValueStorageCost,
    /// Whether the entry's chunk-blob share is charged as added storage at
    /// its own append (so the later blob write can be a replacement).
    prepay_chunk_share: bool,
}

#[cfg(feature = "storage")]
impl AppendStorageAccounting {
    /// The entry's chunk-blob share to charge as added storage at its append:
    /// its own bytes, or nothing when the blob is charged in full at
    /// compaction instead.
    pub fn prepaid_chunk_bytes(&self, value_len: usize) -> u32 {
        if self.prepay_chunk_share {
            u32::try_from(value_len).unwrap_or(u32::MAX)
        } else {
            0
        }
    }
}

/// The storage accounting an append uses under `grove_version`.
#[cfg(feature = "storage")]
pub(crate) fn append_storage_accounting(
    grove_version: &GroveVersion,
) -> Result<AppendStorageAccounting, BulkAppendError> {
    match grove_version
        .bulk_append_tree_versions
        .cost
        .append_storage_accounting
    {
        0 => Ok(v0::append_storage_accounting()),
        1 => Ok(v1::append_storage_accounting()),
        version => Err(BulkAppendError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "BulkAppendTree append storage accounting".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Hashes to report for a compacting append.
///
/// `leaf_count` is the MMR leaf count BEFORE the push (what
/// `hash_count_for_push` expects); `mmr_size_after_push` is the size the MMR
/// reached, which determines how many peaks the compaction's `get_root` had
/// to fold.
pub(crate) fn compaction_hash_count(
    leaf_count: u64,
    mmr_size_after_push: u64,
    grove_version: &GroveVersion,
) -> Result<u32, BulkAppendError> {
    match grove_version
        .bulk_append_tree_versions
        .cost
        .compaction_hash_count
    {
        0 => Ok(v0::compaction_hash_count(leaf_count)),
        1 => Ok(v1::compaction_hash_count(leaf_count, mmr_size_after_push)),
        version => Err(BulkAppendError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "BulkAppendTree compaction hash count".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}
