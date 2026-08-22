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

/// Per-insert upper bound on the dense buffer's hash-record maintenance
/// (root-maintenance version 1, GROVE_V4+) for a buffer of `height`
/// (`chunk_power` for the append-only family, the element's height for a
/// `DenseAppendOnlyFixedSizeTree`).
///
/// An insert at depth `d` (≤ `height - 1`) rewrites the records of the
/// `d + 1` positions on its ancestor path, reads at most the parent's and
/// the sibling's record per level plus — for a slot that already holds a
/// committed value — its own record once to size the write, and hashes
/// `2 + d` times. A buffer filled under GROVE_V1..V3 (no records) is caught
/// up by the first GROVE_V4 insert that needs them: each sibling subtree
/// without a current record is walked from its values (the full walk, in
/// total no more than the version-0 `2 * count` hashes) and its root
/// recorded — at most one more record per level. `catch_up` includes those
/// writes; pass `false` for a tree that can only have been written under
/// GROVE_V4 (the `PrivateDocumentStore`, which activates in V4).
///
/// Reads are bounded here for the callers that bill them (the
/// `CostResult`-returning appends: `PrivateDocumentStore`,
/// `DenseAppendOnlyFixedSizeTree`); the `Result`-returning bulk / commitment
/// appends drop them and bill the hash count alone, as they always have.
/// Record writes reach every caller's cost at commit.
#[cfg(feature = "minimal")]
pub(in crate::batch) struct DenseRecordMaintenanceBound {
    /// Record puts (one seek each at commit).
    pub record_writes: u32,
    /// Record reads (one seek each).
    pub record_reads: u32,
    /// Bytes a record put can add: key, record and framing, per write.
    pub added_bytes: u32,
    /// Bytes a record rewrite replaces, per write.
    pub replaced_bytes: u32,
    /// Bytes the record reads load.
    pub loaded_bytes: u64,
    /// blake3 calls: two for the leaf, one per ancestor level.
    pub hash_calls: u32,
}

#[cfg(feature = "minimal")]
pub(in crate::batch) fn dense_record_maintenance_bound(
    height: u8,
    catch_up: bool,
) -> DenseRecordMaintenanceBound {
    use grovedb_dense_fixed_sized_merkle_tree::HASH_RECORD_LEN;
    let height = height.clamp(1, PHYSICAL_MAX_CHUNK_POWER) as u32;
    let path_records = height;
    let catch_up_records = if catch_up { height - 1 } else { 0 };
    let record_writes = path_records + catch_up_records;
    // Parent + sibling per ancestor level, plus the own-record sizing read.
    let record_reads = 2 * (height - 1) + 1;
    let record_put_bytes = HASH_RECORD_LEN as u32 + PER_PUT_OVERHEAD;
    DenseRecordMaintenanceBound {
        record_writes,
        record_reads,
        added_bytes: record_writes * record_put_bytes,
        replaced_bytes: record_writes * record_put_bytes,
        loaded_bytes: record_reads as u64 * HASH_RECORD_LEN as u64,
        hash_calls: 2 + (height - 1),
    }
}

/// Bytes loaded when reading the stored `CommitmentTree` element sans
/// flags: the serialized fields (variant, varint total count, chunk
/// power, flags option) plus the Merk node framing (hashes, key,
/// length prefixes) — measured ~87, with margin. Caller-supplied flags
/// are bounded separately via `element_flags_load_bound`. The
/// `BulkAppendTree` and `PrivateDocumentStore` elements are no larger
/// (the store adds a 4-byte entry size), so their arms reuse it.
#[cfg(feature = "minimal")]
const CT_ELEMENT_LOAD_BASE: u32 = 256;

/// Upper-bound cost of the append work a single `CommitmentTreeInsert`
/// performs outside the parent Merk (which is charged separately via
/// `average/worst_case_merk_replace_tree`): frontier I/O and Sinsemilla
/// hashing, the note write into the dense buffer (with its per-append
/// root recompute), and a full epoch compaction (chunk-blob write plus
/// MMR merge cascade).
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
///
/// The storage terms follow the GROVE_V4 accounting of the append-only
/// family (issue #822), which charges each note's permanent bytes once
/// and reports write churn as replacement — see the field comments.
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

    // The dense buffer's hash records (GROVE_V4 root maintenance): the
    // ancestor-path rewrites every append performs, plus the catch-up a
    // buffer filled under GROVE_V3 pays once. Their reads are not billed
    // by the commitment tree's append (see `dense_record_maintenance_bound`).
    let records = dense_record_maintenance_bound(chunk_power, true);

    // Chunk-blob framing per blob: the fixed-format header (format byte,
    // entry count, entry size) inside the MMR leaf envelope (flag, hash,
    // length) — 9 + 37 — with margin. Every note in a commitment tree has
    // the same size, so its blobs always take the fixed format, which
    // carries no per-entry framing.
    const CHUNK_BLOB_OVERHEAD: u64 = 64;
    // An MMR internal node: 1 (flag) + 32 (hash).
    const MMR_INTERNAL_NODE_SIZE: u64 = 33;

    // The epoch term multiplies the entry size by up to 2^16, which
    // overflows u32 for hand-built ops with oversized payloads (the op
    // is public; the apply path only rejects wrong-sized payloads
    // later). Sum in u64 and saturate at u32::MAX — a wrapped figure
    // would silently UNDER-estimate, the exact failure this model exists
    // to prevent, while a saturated one merely over-reserves for an op
    // the apply would reject anyway.
    //
    // Added storage — what this append makes the database permanently
    // larger by:
    // - the note's dense-buffer slot when written for the first time
    //   (epoch 1), key included; later epochs rewrite it (replaced below),
    // - the note's share of the eventual chunk blob (its own bytes),
    //   charged at every append so the blob is a replacement when it lands,
    // - the frontier's very first save (key + value); later saves only
    //   add growth (at most one ommer), which this term dominates,
    // - on compaction: the blob's framing beyond the prepaid entry bytes,
    //   and the MMR merge cascade's internal nodes,
    // - the buffer's hash records written for the first time (the new
    //   leaf's in epoch 1; on a GROVE_V3 → V4 catch-up, every record the
    //   insert derives).
    let added_bytes_u64: u64 = (entry_size + PER_PUT_OVERHEAD as u64)
        + entry_size
        + (MAX_FRONTIER_SIZE as u64 + PER_PUT_OVERHEAD as u64)
        + (CHUNK_BLOB_OVERHEAD + PER_PUT_OVERHEAD as u64)
        + FRONTIER_DEPTH as u64 * (MMR_INTERNAL_NODE_SIZE + PER_PUT_OVERHEAD as u64)
        + records.added_bytes as u64;
    // Replaced storage — bytes rewritten over bytes that were already
    // paid for:
    // - the note's buffer slot from epoch 2 on,
    // - the re-serialized frontier (up to MAX_FRONTIER_SIZE),
    // - on compaction: the whole epoch's entry bytes, which the chunk blob
    //   supersedes and every append already charged as added,
    // - the buffer's hash records rewritten along the ancestor path.
    let replaced_bytes_u64: u64 = (entry_size + PER_PUT_OVERHEAD as u64)
        + (MAX_FRONTIER_SIZE as u64 + PER_PUT_OVERHEAD as u64)
        + epoch_size as u64 * entry_size
        + records.replaced_bytes as u64;

    OperationCost {
        // 3 reads (CommitmentTree element, frontier, and the committed
        // note slot a rewrite is sized against) and up to 3 + FRONTIER_DEPTH
        // writes (note entry, frontier, chunk blob, MMR internal nodes),
        // plus the hash-record writes (a compacting append writes no
        // records and a buffered one no MMR nodes, so the two never stack;
        // summed here as a bound).
        seek_count: 6 + FRONTIER_DEPTH + records.record_writes,
        storage_cost: StorageCost {
            added_bytes: u32::try_from(added_bytes_u64).unwrap_or(u32::MAX),
            // The parent-Merk node replacement is charged by the
            // replace_tree part; this is the append's own churn.
            replaced_bytes: u32::try_from(replaced_bytes_u64).unwrap_or(u32::MAX),
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // Reads: the stored CommitmentTree element (fixed serialized
        // fields + Merk node framing, plus the caller-supplied flags
        // bound) + the serialized frontier + the committed note the
        // rewritten buffer slot holds (epoch 2 on).
        storage_loaded_bytes: (CT_ELEMENT_LOAD_BASE
            + element_flags_load_bound
            + MAX_FRONTIER_SIZE
            + PER_PUT_OVERHEAD) as u64
            + entry_size,
        // Blake3: up to a full-buffer walk — two hashes per filled slot.
        // From GROVE_V4 a buffered append hashes only its ancestor path
        // (`records.hash_calls`), but a buffer filled under GROVE_V3 pays
        // the walk once when its first V4 append catches its records up,
        // and that insert is admitted under this bound, so the walk stays.
        // Plus on compaction the chunk-leaf hash and MMR merge cascade,
        // plus the bulk state root and the ct_state binding hash.
        hash_node_calls: (2 * (epoch_size - 1)).max(records.hash_calls) + FRONTIER_DEPTH + 4,
        sinsemilla_hash_calls: MAX_SINSEMILLA_HASHES_PER_APPEND,
    }
}

/// Upper-bound cost of the append work a single `PrivateDocumentStoreInsert`
/// performs outside the parent Merk (charged separately via
/// `average/worst_case_merk_replace_tree`): the entry write into the dense
/// buffer with its hash-record maintenance (GROVE_V4 root maintenance — the
/// store activates in V4, so every store's buffer has records and none ever
/// pays a V3 catch-up walk), and a full epoch compaction (the epoch read back
/// position by position, the chunk blob written and pushed through the MMR,
/// the blob read back as a peak when the MMR root is bagged).
///
/// `chunk_power` is the store's epoch scale: the average-case estimator
/// requires it declared in the store's own layer
/// (`TreeType::PrivateDocumentStore(chunk_power)`), the worst-case estimator
/// passes [`PHYSICAL_MAX_CHUNK_POWER`]. `entry_size` is the committed entry
/// size — `entry.len()` IS it, the append path rejects any other length.
/// `element_flags_load_bound` bounds the caller-supplied flags the
/// preprocessing read of the stored element loads (the average-case
/// estimator derives it from the parent layer's declared flags size, the
/// worst-case estimator passes the largest Merk value size).
///
/// A genuine UPPER BOUND over every position, shared by both estimators for
/// the same reason as [`commitment_tree_insert_op_cost`]: the expensive
/// positions (the deepest ancestor path, the compaction) are deterministic
/// and adversary-reachable, so an "average" is not a meaningful bound, and
/// two differing models would be silently non-interchangeable (issue #812).
/// A buffered append hashes its ancestor path and writes no MMR node; a
/// compacting append reads the epoch back and writes no record — never both,
/// summed here as a bound.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn private_document_store_insert_op_cost(
    entry_size: u32,
    chunk_power: u8,
    element_flags_load_bound: u32,
) -> OperationCost {
    use grovedb_dense_fixed_sized_merkle_tree::HASH_RECORD_LEN;
    let chunk_power = chunk_power.clamp(1, PHYSICAL_MAX_CHUNK_POWER);
    let epoch_entries: u32 = 1u32 << chunk_power;
    // The buffer holds `2^chunk_power - 1` positions; a compacting append
    // reads every one of them back to build the chunk blob.
    let compaction_reads: u32 = epoch_entries - 1;
    let records = dense_record_maintenance_bound(chunk_power, false);
    /// MMR push merges plus root bagging (bounded by the 64-bit position
    /// space).
    const MAX_MMR_MERGES: u32 = 65;
    const MAX_MMR_READS: u32 = 64;
    /// Bulk state root + composite pds_state root + the committed-config
    /// hash paid when the store is opened.
    const ROOT_AND_CONFIG_HASHES: u32 = 3;
    // A compacted epoch is not stored as a bare payload: the chunk blob
    // carries a 9-byte header, sits inside a 37-byte MMR leaf envelope, and
    // every internal MMR node the push creates costs a further 33 bytes.
    const CHUNK_HEADER_BYTES: u32 = 9;
    const MMR_LEAF_ENVELOPE_BYTES: u32 = 37;
    const MMR_INTERNAL_NODE_BYTES: u32 = 33;
    const MMR_SERIALIZATION_OVERHEAD: u32 =
        CHUNK_HEADER_BYTES + MMR_LEAF_ENVELOPE_BYTES + MMR_INTERNAL_NODE_BYTES * MAX_MMR_MERGES;
    // `entry_size` is capped at `u16::MAX` at every creation site precisely
    // so this product stays representable in the u32 `added_bytes` field.
    let max_compaction_blob = epoch_entries
        .saturating_mul(entry_size)
        .saturating_add(MMR_SERIALIZATION_OVERHEAD);
    // A NonCounted-wrapped store serializes one byte wider, and the apply
    // path preserves that wrapper. Neither the op nor the declared layer
    // records whether this store is wrapped, so charge the byte
    // unconditionally.
    const NON_COUNTED_WRAPPER_BYTE: u32 = 1;
    OperationCost {
        // Writes: the buffer entry, the chunk blob and the MMR nodes; reads:
        // the stored element, the committed slot value a rewrite is sized
        // against, the position-0 record for the state root, the MMR
        // siblings, the epoch read back on compaction, and the record reads;
        // plus the record writes.
        seek_count: (1 + 1 + MAX_MMR_MERGES)
            .saturating_add(3)
            .saturating_add(MAX_MMR_READS)
            .saturating_add(compaction_reads)
            .saturating_add(records.record_reads)
            .saturating_add(records.record_writes),
        storage_cost: StorageCost {
            // The buffer slot (new in epoch 1; a rewrite later, replaced
            // below), the entry's chunk-blob share (GROVE_V4 accounting,
            // issue #822), the blob's framing, the wrapper byte, and the
            // records written for the first time.
            added_bytes: entry_size
                .saturating_add(entry_size)
                .saturating_add(MMR_SERIALIZATION_OVERHEAD)
                .saturating_add(NON_COUNTED_WRAPPER_BYTE)
                .saturating_add(records.added_bytes),
            // The slot rewrite from epoch 2 on, the compaction blob (a
            // replacement of the epoch's prepaid entry bytes), and the
            // records rewritten along the path.
            replaced_bytes: entry_size
                .saturating_add(max_compaction_blob)
                .saturating_add(records.replaced_bytes),
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // The stored element (fixed fields + Merk framing, with the flags
        // bound), the committed slot value, the epoch read back on
        // compaction, the blob read back as an MMR peak, the MMR siblings,
        // the record reads and the root record.
        storage_loaded_bytes: (CT_ELEMENT_LOAD_BASE + element_flags_load_bound) as u64
            + entry_size as u64
            + compaction_reads as u64 * entry_size as u64
            + max_compaction_blob as u64
            + (MMR_INTERNAL_NODE_BYTES * MAX_MMR_READS) as u64
            + records.loaded_bytes
            + HASH_RECORD_LEN as u64,
        // A buffered append's ancestor path, a compacting append's chunk
        // leaf hash and MMR cascade, and the roots.
        hash_node_calls: records.hash_calls + 1 + MAX_MMR_MERGES + ROOT_AND_CONFIG_HASHES,
        sinsemilla_hash_calls: 0,
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
