//! Estimated costs

#[cfg(feature = "minimal")]
use std::collections::HashMap;

#[cfg(feature = "minimal")]
use grovedb_costs::{
    storage_cost::{removal::StorageRemovedBytes, StorageCost},
    OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::{
    average_case_costs::EstimatedLayerInformation, worst_case_costs::WorstCaseLayerInformation,
};

#[cfg(feature = "minimal")]
use crate::batch::KeyInfoPath;

#[cfg(feature = "minimal")]
pub mod average_case_costs;
#[cfg(feature = "minimal")]
pub mod worst_case_costs;

/// Cost-overhead in serialized bytes when a tree element will be
/// rebuilt wrapped in `Element::NonCounted`, `Element::NotSummed`, or
/// `Element::NotCountedOrSummed`. Each wrapper prepends one
/// discriminant byte to the on-disk payload. The three wrappers are
/// mutually exclusive on any element, so at most one of the input
/// flags is ever true.
#[cfg(feature = "minimal")]
#[inline]
pub(in crate::batch) fn wrapper_overhead_for(
    non_counted: bool,
    not_summed: bool,
    not_counted_or_summed: bool,
) -> u32 {
    if non_counted || not_summed || not_counted_or_summed {
        1
    } else {
        0
    }
}

// ── CommitmentTreeInsert estimation model ───────────────────────────────
//
// Every constant below is an UPPER BOUND, not an average. Downstream
// consumers (Dash Platform admission control) use the estimate as the
// bound that decides whether a transaction is adequately funded, then
// re-meter with the real cost during execution; `estimated >= actual`
// is the invariant they rely on. The expensive appends are not a rare
// tail: the Sinsemilla ommer cascade is maximal exactly at positions
// 2^k - 1 and epoch compaction fires exactly every 2^chunk_power-th
// append, both deterministic and cheaply reachable by an adversary
// choosing when to append. See issue #812.

/// Depth of the Sinsemilla note-commitment frontier (Orchard's
/// `NOTE_COMMITMENT_TREE_DEPTH`, 32). All frontier-related bounds are
/// derived from this so a depth change cannot silently reintroduce an
/// estimation gap.
#[cfg(feature = "minimal")]
const FRONTIER_DEPTH: u32 = grovedb_commitment_tree::NOTE_COMMITMENT_TREE_DEPTH as u32;

/// Upper bound on Sinsemilla hash calls for a single append:
/// `FRONTIER_DEPTH` for the leaf-to-root walk plus up to
/// `FRONTIER_DEPTH` ommer merges (`trailing_ones(position)`, maximal at
/// positions `2^k - 1`).
#[cfg(feature = "minimal")]
pub const MAX_SINSEMILLA_HASHES_PER_APPEND: u32 = FRONTIER_DEPTH + FRONTIER_DEPTH;

/// Upper bound on the serialized frontier size:
/// 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer count) + 32 bytes per
/// ommer, with at most `FRONTIER_DEPTH` ommers.
#[cfg(feature = "minimal")]
pub const MAX_FRONTIER_SIZE: u32 = 1 + 8 + 32 + 1 + FRONTIER_DEPTH * 32;

/// Physical ceiling on `chunk_power`: the dense buffer's `u16` count
/// limits the underlying tree height to 16, and `BulkAppendTree`
/// construction rejects anything larger, so no tree beyond this can
/// function on disk. The worst-case estimator (which has no channel for
/// the tree's declared shape) charges this ceiling, and declared chunk
/// powers are clamped here to keep the `1 << chunk_power` epoch
/// arithmetic in range.
#[cfg(feature = "minimal")]
pub const PHYSICAL_MAX_CHUNK_POWER: u8 = 16;

/// Per-put storage overhead charged on data-storage writes: the 32-byte
/// blake3 path prefix, the logical key (dense positions, MMR indices,
/// `__ct_data__`), and the key/value varint length prefixes.
#[cfg(feature = "minimal")]
const PER_PUT_OVERHEAD: u32 = 50;

/// Bytes loaded when reading the stored `CommitmentTree` element sans
/// flags: the serialized fields (variant, varint total count, chunk
/// power, flags option) plus the Merk node framing (hashes, key,
/// length prefixes) — measured ~87, with margin. Caller-supplied flags
/// are bounded separately via `element_flags_load_bound`.
#[cfg(feature = "minimal")]
const CT_ELEMENT_LOAD_BASE: u32 = 256;

/// Upper-bound cost of the append work a single `CommitmentTreeInsert`
/// performs outside the parent Merk (which is charged separately via
/// `average/worst_case_merk_replace_tree`): frontier I/O and Sinsemilla
/// hashing, the note write into the dense buffer (with its per-append
/// root recompute), and a full epoch compaction (chunk-blob write plus
/// MMR merge cascade).
///
/// Storage bytes follow the append-only family's storage accounting v1
/// (`bulk_append_tree_versions.cost.storage_accounting`, the only report
/// GROVE_V4 — where this arm is selected — uses): an append **adds** its
/// entry plus its amortized share of the chunk blob's framing, and any
/// growth of the frontier; the compaction's blob and the frontier rewrite
/// are **replaced** bytes (the blob supersedes the pre-paid buffer entries,
/// the frontier overwrites its previous serialization), so the bound
/// carries a replaced-bytes term sized for a full epoch and the frontier at
/// its maximum, and its added-bytes term no longer scales with the epoch.
///
/// `chunk_power` is the tree's epoch scale: the average-case estimator
/// requires it declared in the tree's own layer in the estimation paths
/// (`TreeType::CommitmentTree(chunk_power)`) and errors when it is
/// missing; the worst-case estimator, which has no declaration channel,
/// passes [`PHYSICAL_MAX_CHUNK_POWER`]. Values above the physical
/// ceiling are clamped to it.
///
/// `element_flags_load_bound` bounds the caller-supplied flags on the
/// stored `CommitmentTree` element, which the preprocessing read loads:
/// the average-case estimator derives it from the parent layer's
/// declared flags size (the same metadata the parent-node replace
/// uses), the worst-case estimator passes the largest Merk value size.
///
/// Used by BOTH the average-case and the worst-case estimators. The
/// append cost is position-dependent and the position is
/// adversary-controlled, so an "average" here is not a meaningful
/// bound; making the two estimators differ would make them silently
/// non-interchangeable, which is a consensus fault for admission
/// control (see issue #812).
#[cfg(feature = "minimal")]
pub(in crate::batch) fn commitment_tree_insert_op_cost(
    payload_len: u32,
    chunk_power: u8,
    element_flags_load_bound: u32,
) -> OperationCost {
    // A stored note entry: cmx (32) || rho (32) || cv_net (32) || payload.
    let entry_size = 96u64 + payload_len as u64;

    // Epoch size for the compaction and dense-recompute bounds. Clamped
    // to the physical ceiling so hand-built layer information cannot
    // overflow the shift.
    let epoch_size: u32 = 1u32 << chunk_power.min(PHYSICAL_MAX_CHUNK_POWER);

    // Chunk-blob serialization overhead per entry (length prefix) and
    // per blob (entry count, MMR leaf node framing).
    const CHUNK_ENTRY_OVERHEAD: u64 = 16;
    const CHUNK_BLOB_OVERHEAD: u64 = 64;
    // An MMR internal node: 1 (flag) + 32 (hash).
    const MMR_INTERNAL_NODE_SIZE: u64 = 33;
    // The frontier's serialization grows by at most one 32-byte ommer per
    // append (popcount rises by at most one), and its first-ever save of
    // a one-leaf frontier is 42 bytes; 64 bounds both, with the varint.
    const FRONTIER_ADDED_PER_APPEND: u64 = 64;

    // Added bytes: the entry with its amortized blob-framing share, any
    // frontier growth, the blob's own residual (header beyond what the
    // entries pre-paid), and the MMR merge cascade's internal nodes. None
    // of these scale with the epoch: the epoch-sized blob copy is a
    // replacement (below), not new storage.
    let added_bytes_u64: u64 = (entry_size + CHUNK_ENTRY_OVERHEAD + PER_PUT_OVERHEAD as u64)
        + (FRONTIER_ADDED_PER_APPEND + PER_PUT_OVERHEAD as u64)
        + (CHUNK_BLOB_OVERHEAD + PER_PUT_OVERHEAD as u64)
        + FRONTIER_DEPTH as u64 * (MMR_INTERNAL_NODE_SIZE + PER_PUT_OVERHEAD as u64);
    // Replaced bytes: on the compacting append, the whole epoch's entries
    // (each with its framing share) are superseded by the blob; on every
    // append the previous frontier serialization is overwritten. The
    // epoch term multiplies the entry size by up to 2^16, which overflows
    // u32 for hand-built ops with oversized payloads (the op is public;
    // the apply path only rejects wrong-sized payloads later). Sum in u64
    // and saturate at u32::MAX — a wrapped figure would silently
    // UNDER-estimate, the exact failure this model exists to prevent,
    // while a saturated one merely over-reserves for an op the apply
    // would reject anyway.
    let replaced_bytes_u64: u64 = epoch_size as u64 * (entry_size + CHUNK_ENTRY_OVERHEAD)
        + (MAX_FRONTIER_SIZE as u64 + PER_PUT_OVERHEAD as u64);

    OperationCost {
        // 2 reads (CommitmentTree element + frontier) and up to
        // 3 + FRONTIER_DEPTH writes (note entry, frontier, chunk blob,
        // MMR internal nodes).
        seek_count: 5 + FRONTIER_DEPTH,
        storage_cost: StorageCost {
            // Data-storage writes are charged as added bytes (the
            // commit path has no previous-size information for them,
            // and dense/MMR keys are new within an epoch), so the whole
            // write volume lands here:
            // - the note entry into the dense buffer,
            // - the re-serialized frontier (grows toward
            //   MAX_FRONTIER_SIZE),
            // - on compaction: the epoch's chunk blob (every entry is
            //   re-written once into the blob) and the MMR merge
            //   cascade's internal nodes.
            added_bytes: u32::try_from(added_bytes_u64).unwrap_or(u32::MAX),
            // The blob copy and the frontier rewrite; the parent-Merk node
            // replacement is charged by the replace_tree part.
            replaced_bytes: u32::try_from(replaced_bytes_u64).unwrap_or(u32::MAX),
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // Reads: the stored CommitmentTree element (fixed serialized
        // fields + Merk node framing, plus the caller-supplied flags
        // bound) + the serialized frontier.
        storage_loaded_bytes: (CT_ELEMENT_LOAD_BASE
            + element_flags_load_bound
            + MAX_FRONTIER_SIZE
            + PER_PUT_OVERHEAD) as u64,
        // Blake3: the dense buffer's root recompute visits every filled
        // slot (2 hashes each, up to a full buffer), plus on compaction
        // the chunk-leaf hash and MMR merge cascade, plus the bulk
        // state root and the ct_state binding hash.
        hash_node_calls: 2 * (epoch_size - 1) + FRONTIER_DEPTH + 4,
        sinsemilla_hash_calls: MAX_SINSEMILLA_HASHES_PER_APPEND,
    }
}

/// Estimated costs types
#[cfg(feature = "minimal")]
pub enum EstimatedCostsType {
    /// Average cast estimated costs type
    AverageCaseCostsType(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    /// Worst case estimated costs type
    WorstCaseCostsType(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
