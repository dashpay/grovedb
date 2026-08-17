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

/// Largest `chunk_power` the CommitmentTreeInsert estimate covers
/// (2^10 = 1024-entry epochs, the recommended default). The dense
/// buffer's per-append root recompute and the epoch-compaction blob
/// both scale with `2^chunk_power`, which the op does not carry, so the
/// estimator charges this documented cap. Estimates for trees created
/// with a larger `chunk_power` are NOT upper bounds.
#[cfg(feature = "minimal")]
pub const MAX_ESTIMATED_CHUNK_POWER: u32 = 10;

/// Epoch size implied by [`MAX_ESTIMATED_CHUNK_POWER`].
#[cfg(feature = "minimal")]
const MAX_EPOCH_SIZE: u32 = 1 << MAX_ESTIMATED_CHUNK_POWER;

/// Per-put storage overhead charged on data-storage writes: the 32-byte
/// blake3 path prefix, the logical key (dense positions, MMR indices,
/// `__ct_data__`), and the key/value varint length prefixes.
#[cfg(feature = "minimal")]
const PER_PUT_OVERHEAD: u32 = 50;

/// Upper-bound cost of the append work a single `CommitmentTreeInsert`
/// performs outside the parent Merk (which is charged separately via
/// `average/worst_case_merk_replace_tree`): frontier I/O and Sinsemilla
/// hashing, the note write into the dense buffer (with its per-append
/// root recompute), and a full epoch compaction (chunk-blob write plus
/// MMR merge cascade).
///
/// Used by BOTH the average-case and the worst-case estimators. The
/// append cost is position-dependent and the position is
/// adversary-controlled, so an "average" here is not a meaningful
/// bound; making the two estimators differ would make them silently
/// non-interchangeable, which is a consensus fault for admission
/// control (see issue #812).
#[cfg(feature = "minimal")]
pub(in crate::batch) fn commitment_tree_insert_op_cost(payload_len: u32) -> OperationCost {
    // A stored note entry: cmx (32) || rho (32) || cv_net (32) || payload.
    let entry_size = 96 + payload_len;

    // Chunk-blob serialization overhead per entry (length prefix) and
    // per blob (entry count, MMR leaf node framing).
    const CHUNK_ENTRY_OVERHEAD: u32 = 16;
    const CHUNK_BLOB_OVERHEAD: u32 = 64;
    // An MMR internal node: 1 (flag) + 32 (hash).
    const MMR_INTERNAL_NODE_SIZE: u32 = 33;

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
            added_bytes: (entry_size + PER_PUT_OVERHEAD)
                + (MAX_FRONTIER_SIZE + PER_PUT_OVERHEAD)
                + (MAX_EPOCH_SIZE * (entry_size + CHUNK_ENTRY_OVERHEAD)
                    + CHUNK_BLOB_OVERHEAD
                    + PER_PUT_OVERHEAD)
                + FRONTIER_DEPTH * (MMR_INTERNAL_NODE_SIZE + PER_PUT_OVERHEAD),
            // The parent-Merk node replacement is charged by the
            // replace_tree part; the append itself replaces nothing.
            replaced_bytes: 0,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // Reads: the CommitmentTree element (generous margin for
        // caller-supplied element flags) + the serialized frontier.
        storage_loaded_bytes: (512 + MAX_FRONTIER_SIZE + PER_PUT_OVERHEAD) as u64,
        // Blake3: the dense buffer's root recompute visits every filled
        // slot (2 hashes each, up to a full buffer), plus on compaction
        // the chunk-leaf hash and MMR merge cascade, plus the bulk
        // state root and the ct_state binding hash.
        hash_node_calls: 2 * (MAX_EPOCH_SIZE - 1) + FRONTIER_DEPTH + 4,
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
