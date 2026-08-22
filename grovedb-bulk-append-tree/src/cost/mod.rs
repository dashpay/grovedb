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
//!   reports every buffer write (slot and path record, which are churn: the
//!   buffer is rewritten each epoch and is not the entry's long-term
//!   storage) as replacements, never as growth; it charges every append the
//!   fixed model — the buffer's root-maintenance model plus the compaction
//!   amortized over the epoch — and bills the compacting append nothing
//!   extra (the blob and MMR nodes it writes are prepaid).

mod v0;
mod v1;

#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::LeafValueStorageCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::BulkAppendError;

/// How the dense-buffer slot write — and the path record written beside it
/// — is sized for the storage cost layer.
#[cfg(feature = "storage")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotRewriteAccounting {
    /// Every slot write is new storage (no cost information on the put).
    AsNew,
    /// The buffer is churn: every slot and record write is reported as an
    /// in-place replacement of its own size — nothing added, no key charged,
    /// nothing read to size it. The entry's long-term bytes are its chunk-blob
    /// share, prepaid at its append.
    Churn,
}

/// How an append's data-storage writes are reported to the storage cost
/// layer. Selected per grove version by [`append_storage_accounting`].
#[cfg(feature = "storage")]
#[derive(Clone, Copy)]
pub(crate) struct AppendStorageAccounting {
    /// How the dense-buffer slot (and record) write is sized.
    pub slot_rewrite: SlotRewriteAccounting,
    /// How the chunk-blob leaf is reported when the MMR overlay is flushed.
    pub chunk_leaf: LeafValueStorageCost,
    /// Whether the entry's chunk-blob share is charged as added storage at
    /// its own append (so the later blob write can be a replacement).
    prepay_chunk_share: bool,
    /// Whether an append is charged the FIXED per-append model instead of
    /// the work of its particular position: the dense buffer's
    /// root-maintenance model (`v1_insert_model_cost`, reads and hashes) on
    /// every append — buffered or compacting — plus the compaction amortized
    /// over the epoch (one blake3 per append, the entry's own bytes as the
    /// blob rewrite it will be part of, and a share of the blob framing and
    /// MMR nodes as added storage), with the compacting append's own work
    /// (the read-back, the chunk-leaf hash, the MMR merges) billed nothing
    /// extra. The shipped accounting bills the dense walk's hashes and the
    /// compaction's hashes where they happen and drops the reads.
    pub fixed_model: bool,
}

/// The most blake3 calls one compaction can perform: the chunk-leaf hash,
/// the MMR push's merges (one per trailing one bit of the chunk index) and
/// the root bagging (one per peak beyond the first). MMR positions are
/// 32-bit keys, so the chunk MMR never exceeds 2^31 leaves: at most 31
/// merges and 31 bagging folds — bounded by 32 each here.
pub const MAX_COMPACTION_HASHES_PER_CHUNK: u32 = 1 + 32 + 32;

/// The blake3 calls a compaction performs, amortized over the epoch it
/// serves and charged on every append under the fixed model: the per-chunk
/// bound spread over `2^chunk_power` appends, rounded up — one per append
/// from `chunk_power` 7 (so at the shielded pool's 11), and 33 at the
/// smallest `chunk_power`. Charging the bound rather than the average keeps
/// every prefix of the tree's life prepaid: a chunk's charge
/// (`2^chunk_power` × this) is never below its actual work, so no run of
/// small or late epochs can fall behind.
pub fn amortized_compaction_hashes(chunk_power: u8) -> u32 {
    MAX_COMPACTION_HASHES_PER_CHUNK.div_ceil(1u32 << chunk_power.min(16) as u32)
}

/// The largest per-append compaction hash share the type permits — the
/// smallest epoch, `chunk_power` 1.
pub fn max_amortized_compaction_hashes() -> u32 {
    amortized_compaction_hashes(1)
}

/// The most puts one compaction can issue at commit: the chunk blob, the
/// MMR internal nodes its push creates (one per trailing one bit of the
/// chunk index — at most 31 with 32-bit MMR keys, bounded by 32 here) and
/// the persisted MMR root. Under the fixed model every one of them is
/// prepaid (`KeyValueStorageCost::prepaid()`, no seek at commit) and their
/// seeks are charged on every append as this bound over the epoch.
pub const MAX_COMPACTION_PUTS_PER_CHUNK: u32 = 1 + 32 + 1;

/// The seeks a compaction's commit-time puts cost, amortized over the epoch
/// and charged on every append under the fixed model: the per-chunk bound
/// over `2^chunk_power` appends, rounded up — one per append from
/// `chunk_power` 6 (so at the shielded pool's 11), 17 at the smallest.
/// Charged as a bound so every prefix of the tree's life stays prepaid.
pub fn amortized_compaction_seeks(chunk_power: u8) -> u32 {
    MAX_COMPACTION_PUTS_PER_CHUNK.div_ceil(1u32 << chunk_power.min(16) as u32)
}

/// The largest per-append compaction seek share the type permits — the
/// smallest epoch, `chunk_power` 1.
pub fn max_amortized_compaction_seeks() -> u32 {
    amortized_compaction_seeks(1)
}

/// The puts a buffered append issues at commit — its slot and its path
/// record — which a compacting append, writing neither (its puts are all
/// prepaid), is charged all the same under the fixed model, so its seek
/// figure is every other append's.
pub const BUFFER_CHURN_PUTS: u32 = 2;

/// The per-entry framing a variable-format chunk blob carries: a four-byte
/// length prefix before every entry (`serialize_variable`). A tree whose
/// entries are all one size serializes the fixed format (no per-entry
/// framing); the bulk-append tree cannot know which format an epoch will
/// take until it compacts, so an owner that does not declare a fixed entry
/// size (`BulkAppendTree::with_fixed_entry_size`) is charged this bound on
/// every entry.
pub const VARIABLE_ENTRY_FRAMING_BYTES: u32 = 4;

/// Bytes a compaction adds beyond the epoch's entry bytes, amortized over
/// the epoch: the chunk blob's framing (its MMR leaf key with the 32-byte
/// path prefix, the leaf envelope, the blob header and length varints —
/// ≈ 88 bytes) and the MMR internal node the push creates on average (one
/// per chunk: key, 33-byte node, length — 71 bytes).
pub const COMPACTION_OVERHEAD_BYTES_PER_EPOCH: u32 = 88 + 71;

/// The compaction overhead an append is charged as added storage under the
/// fixed model: the epoch's share of [`COMPACTION_OVERHEAD_BYTES_PER_EPOCH`],
/// rounded up.
pub fn amortized_compaction_added_bytes(epoch_size: u64) -> u32 {
    (COMPACTION_OVERHEAD_BYTES_PER_EPOCH as u64).div_ceil(epoch_size.max(1)) as u32
}

#[cfg(feature = "storage")]
impl AppendStorageAccounting {
    /// The entry's chunk-blob share to charge as added storage at its append:
    /// its own bytes plus the per-entry blob framing the owner has not ruled
    /// out (`entry_framing`: [`VARIABLE_ENTRY_FRAMING_BYTES`] unless the tree
    /// declares a fixed entry size), or nothing when the blob is charged in
    /// full at compaction instead.
    pub fn prepaid_chunk_bytes(&self, value_len: usize, entry_framing: u32) -> u32 {
        if self.prepay_chunk_share {
            u32::try_from(value_len)
                .unwrap_or(u32::MAX)
                .saturating_add(entry_framing)
        } else {
            0
        }
    }

    /// The compaction overhead — blob framing and MMR node — an append is
    /// charged as added storage under the fixed model: the epoch's share,
    /// rounded up (one byte at `chunk_power` 11). Nothing when the
    /// compaction is charged where it happens.
    pub fn amortized_compaction_added_bytes(&self, epoch_size: u64) -> u32 {
        if self.fixed_model {
            amortized_compaction_added_bytes(epoch_size)
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

/// Hashes to report for a compacting append, on top of what every append
/// reports.
///
/// `leaf_count` is the MMR leaf count BEFORE the push (what
/// `hash_count_for_push` expects); `mmr_size_after_push` is the size the MMR
/// reached, which determines how many peaks the compaction's `get_root` had
/// to fold. Version 1 reports nothing here: the compaction is amortized into
/// every append ([`amortized_compaction_hashes`]).
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
