//! Versioned cost accounting for the bulk-append tree.
//!
//! Only the reported hash count is versioned; chunk bytes, roots and stored
//! state are identical under every version. This gate reaches a live fee —
//! `CommitmentTree` adds the compaction hash count straight into its own
//! `hash_node_calls` — so the corrected figure arrives as a new version rather
//! than replacing the old one.

mod v0;
mod v1;

use grovedb_costs::storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use integer_encoding::VarInt;

use crate::BulkAppendError;

/// Per-entry framing the chunk blob adds over the raw entry bytes (length
/// prefix and per-entry overhead), charged to each append as its amortized
/// share of the blob under storage accounting v1 — so that a compacting
/// append's blob put can be reported as replacement of bytes that were
/// already paid for. Kept equal to the estimator's per-entry chunk overhead
/// so the bound and the actual agree on what is pre-paid.
pub const CHUNK_ENTRY_AMORTIZED_BYTES: u32 = 16;

/// Which storage-accounting report the append-only writes use; see
/// `BulkAppendTreeCostVersions::storage_accounting`.
pub(crate) fn storage_accounting_version(
    grove_version: &GroveVersion,
) -> Result<u16, BulkAppendError> {
    match grove_version
        .bulk_append_tree_versions
        .cost
        .storage_accounting
    {
        v @ (0 | 1) => Ok(v),
        version => Err(BulkAppendError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "BulkAppendTree storage accounting".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Cost info for writing one entry into the dense buffer under storage
/// accounting v1: the entry's bytes (plus the value-length varint the
/// storage layer would have counted) and its amortized share of the chunk
/// blob's framing are charged as **added** — each entry's permanent bytes,
/// paid once, by the append that creates it. `new_key` is whether this
/// buffer position has never been written (first epoch); from the second
/// epoch on the position key already exists and only the value is new.
pub(crate) fn buffer_entry_cost_info(value_len: u32, new_key: bool) -> KeyValueStorageCost {
    let added = entry_charge_bytes(value_len);
    KeyValueStorageCost {
        // The key's own bytes are appended by the storage context when
        // `new_node` (it alone knows the prefix).
        key_storage_cost: StorageCost::default(),
        value_storage_cost: StorageCost {
            added_bytes: added,
            replaced_bytes: 0,
            removed_bytes: Default::default(),
        },
        new_node: new_key,
        needs_value_verification: false,
    }
}

/// Cost info for writing the chunk blob a compaction produces, under
/// storage accounting v1.
///
/// The blob supersedes the buffer entries it was built from; their bytes
/// (entry + varint + amortized framing share) were already paid as added
/// by the appends that wrote them — `pre_paid_bytes` — so they are
/// reported as **replaced**. The value that triggered the compaction never
/// entered the buffer (it goes straight into the blob), so this append
/// pays for it here, as added, exactly as a buffered append would have
/// (`compacting_entry_bytes`, the same entry + varint + framing share).
/// Any residual the blob carries beyond the pre-paid bytes (its own
/// header; nothing, when the compact fixed-size format undercuts the
/// per-entry framing pre-payment) is added too. The blob's key is a new
/// MMR position.
pub(crate) fn chunk_blob_cost_info(
    blob_len: u32,
    pre_paid_bytes: u32,
    compacting_entry_bytes: u32,
) -> KeyValueStorageCost {
    let total = blob_len.saturating_add(blob_len.required_space() as u32);
    let replaced = total.min(pre_paid_bytes);
    let residual = total.saturating_sub(pre_paid_bytes);
    KeyValueStorageCost {
        key_storage_cost: StorageCost::default(),
        value_storage_cost: StorageCost {
            added_bytes: compacting_entry_bytes.saturating_add(residual),
            replaced_bytes: replaced,
            removed_bytes: Default::default(),
        },
        new_node: true,
        needs_value_verification: false,
    }
}

/// The bytes an append is charged (as added) for one buffered entry:
/// entry + value-length varint + its amortized chunk-framing share.
pub(crate) fn entry_charge_bytes(value_len: u32) -> u32 {
    value_len
        .saturating_add(value_len.required_space() as u32)
        .saturating_add(CHUNK_ENTRY_AMORTIZED_BYTES)
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
