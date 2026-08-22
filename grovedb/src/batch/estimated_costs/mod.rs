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

/// The dense buffer's fixed per-insert root-maintenance charge under
/// GROVE_V4 (`dense_tree_versions.root_maintenance = 1`) for a tree of
/// `height` (`chunk_power` for the append-only family, the element's height
/// for a `DenseAppendOnlyFixedSizeTree`): what every insert is billed for
/// the buffer — the blake3 calls and path-record reads, averaged over a full
/// buffer and rounded up — whatever its position. Exactly the figure the
/// dense tree returns (`v1_insert_model_cost`), so an estimate built on it
/// is tight, not a bound; the slot and record puts the insert issues are
/// real writes sized by the owner's accounting (see the callers).
///
/// A buffer filled under GROVE_V1..V3 is caught up from its values by its
/// first V4 inserts; that work is real but billed the same model, so no
/// estimator needs a full-buffer walk any more.
#[cfg(feature = "minimal")]
pub(in crate::batch) struct DenseBufferModel {
    /// The model: record reads (seeks + loaded bytes) and blake3 calls.
    pub cost: OperationCost,
    /// Bytes of one path record for this height: what the insert's record
    /// put writes.
    pub record_len: u32,
}

#[cfg(feature = "minimal")]
pub(in crate::batch) fn dense_buffer_model(height: u8) -> DenseBufferModel {
    use grovedb_dense_fixed_sized_merkle_tree::V1InsertModel;
    let model = V1InsertModel::for_height(height.clamp(1, PHYSICAL_MAX_CHUNK_POWER));
    DenseBufferModel {
        cost: model.cost(),
        record_len: model.record_len,
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
/// hashing, the note write into the dense buffer with the buffer's fixed
/// root-maintenance model, and a full epoch compaction (chunk-blob write
/// plus MMR merge cascade).
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
/// family (issue #822): a note's ADDED storage is its long-term footprint —
/// its share of the chunk blob (prepaid at its append), the blob's framing
/// and the MMR nodes, amortized — while the dense buffer (slot and path
/// record, a fixed per-tree scratch area rewritten every epoch) and the
/// frontier are churn, reported as REPLACED bytes only.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn commitment_tree_insert_op_cost(
    payload_len: u32,
    chunk_power: u8,
    element_flags_load_bound: u32,
    amortized_compaction_added: u32,
) -> OperationCost {
    // A stored note entry: cmx (32) || rho (32) || cv_net (32) || payload.
    let entry_size = 96u64 + payload_len as u64;

    // The dense buffer's fixed root-maintenance charge, billed by the
    // commitment tree's append through `storage_accounting_cost`.
    let buffer = dense_buffer_model(chunk_power);
    // The compaction, amortized over the epoch (bulk-append tree fixed
    // model): one blake3 per append, and — `amortized_compaction_added`,
    // supplied by the caller: the declared epoch's share, or the largest
    // share the type permits for a bound — the blob framing and MMR nodes
    // as added storage.
    // The frontier's fixed model: the depth-deep root walk plus the average
    // ommer merge, and the average serialized frontier loaded and replaced.
    let frontier_len = grovedb_commitment_tree::MODEL_FRONTIER_SERIALIZED_LEN;

    // Added storage — the note's long-term footprint: its share of the
    // eventual chunk blob (its own bytes), charged at every append so the
    // blob is prepaid when it lands, plus its share of the blob framing and
    // the MMR nodes. The buffer slot, the path record and the frontier are
    // churn: never added.
    let added_bytes_u64: u64 = entry_size + amortized_compaction_added as u64;
    // Replaced storage — churn:
    // - the note's buffer slot (every epoch, epoch 1 included),
    // - the path record the insert writes (fixed size for the height),
    // - the note's own bytes again, as its part of the blob rewrite the
    //   epoch's compaction performs,
    // - the re-serialized frontier, at the model size.
    let replaced_bytes_u64: u64 = (entry_size + PER_PUT_OVERHEAD as u64)
        + (buffer.record_len as u64 + PER_PUT_OVERHEAD as u64)
        + entry_size
        + (frontier_len as u64 + PER_PUT_OVERHEAD as u64);

    OperationCost {
        // 2 reads (CommitmentTree element, frontier) + the buffer model's
        // record reads, 3 writes (note entry, path record, frontier), and
        // the compaction's commit-time puts amortized over the epoch (its
        // puts are prepaid, so this is the whole of their charge).
        seek_count: 5
            + buffer.cost.seek_count
            + grovedb_bulk_append_tree::amortized_compaction_seeks(chunk_power),
        storage_cost: StorageCost {
            added_bytes: u32::try_from(added_bytes_u64).unwrap_or(u32::MAX),
            // The parent-Merk node replacement is charged by the
            // replace_tree part; this is the append's own churn.
            replaced_bytes: u32::try_from(replaced_bytes_u64).unwrap_or(u32::MAX),
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // Reads: the stored CommitmentTree element (fixed serialized
        // fields + Merk node framing, plus the caller-supplied flags
        // bound) + the model-sized frontier + the buffer model's records.
        storage_loaded_bytes: (CT_ELEMENT_LOAD_BASE + element_flags_load_bound + frontier_len)
            as u64
            + buffer.cost.storage_loaded_bytes,
        // Blake3: the buffer model, the amortized compaction, the bulk
        // state root and the ct_state binding hash — the same at every
        // position.
        hash_node_calls: buffer.cost.hash_node_calls
            + grovedb_bulk_append_tree::amortized_compaction_hashes(chunk_power)
            + 2,
        sinsemilla_hash_calls: grovedb_commitment_tree::MODEL_FRONTIER_APPEND_SINSEMILLA_HASHES,
    }
}

/// Upper-bound cost of the append work a single `PrivateDocumentStoreInsert`
/// performs outside the parent Merk (charged separately via
/// `average/worst_case_merk_replace_tree`): the entry write into the dense
/// buffer with the buffer's fixed root-maintenance model (GROVE_V4; the
/// store activates in V4, so every store's buffer has records), and a full
/// epoch compaction (the epoch read back position by position, the chunk
/// blob written and pushed through the MMR, the blob read back as a peak
/// when the MMR root is bagged).
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
/// position (the compaction) is deterministic and adversary-reachable, so
/// an "average" is not a meaningful bound, and two differing models would
/// be silently non-interchangeable (issue #812). A buffered append is billed
/// the buffer model and writes no MMR node; a compacting append reads the
/// epoch back and writes no slot or record — never both, summed here.
///
/// Storage follows the GROVE_V4 accounting: the entry's added bytes are its
/// long-term footprint (its chunk-blob share, the blob framing and MMR nodes
/// amortized); the buffer slot and path record are churn, replaced only.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn private_document_store_insert_op_cost(
    entry_size: u32,
    chunk_power: u8,
    element_flags_load_bound: u32,
    amortized_compaction_added: u32,
) -> OperationCost {
    let chunk_power = chunk_power.clamp(1, PHYSICAL_MAX_CHUNK_POWER);
    let buffer = dense_buffer_model(chunk_power);
    /// Bulk state root + composite pds_state root + the committed-config
    /// hash paid when the store is opened.
    const ROOT_AND_CONFIG_HASHES: u32 = 3;
    // A NonCounted-wrapped store serializes one byte wider, and the apply
    // path preserves that wrapper. Neither the op nor the declared layer
    // records whether this store is wrapped, so charge the byte
    // unconditionally.
    const NON_COUNTED_WRAPPER_BYTE: u32 = 1;
    OperationCost {
        // Reads: the stored element, the state root's two fixed reads (the
        // persisted MMR root and the last insert's record), the buffer
        // model's record reads; writes: the buffer entry and its path
        // record; plus the compaction's commit-time puts amortized over the
        // epoch.
        seek_count: 3u32
            .saturating_add(buffer.cost.seek_count)
            .saturating_add(2)
            .saturating_add(grovedb_bulk_append_tree::amortized_compaction_seeks(
                chunk_power,
            )),
        storage_cost: StorageCost {
            // The entry's chunk-blob share plus its share of the blob
            // framing and MMR nodes (GROVE_V4 accounting, issue #822), and
            // the wrapper byte.
            added_bytes: entry_size
                .saturating_add(amortized_compaction_added)
                .saturating_add(NON_COUNTED_WRAPPER_BYTE),
            // The buffer slot and the path record (churn, every epoch), and
            // the entry's own bytes as its part of the blob rewrite.
            replaced_bytes: entry_size
                .saturating_add(PER_PUT_OVERHEAD)
                .saturating_add(buffer.record_len)
                .saturating_add(PER_PUT_OVERHEAD)
                .saturating_add(entry_size),
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // The stored element (fixed fields + Merk framing, with the flags
        // bound), the buffer model's records, the persisted MMR root and
        // the root record.
        storage_loaded_bytes: (CT_ELEMENT_LOAD_BASE + element_flags_load_bound) as u64
            + buffer.cost.storage_loaded_bytes
            + 32
            + buffer.record_len as u64,
        // The buffer model, the amortized compaction, and the roots — the
        // same at every position.
        hash_node_calls: buffer.cost.hash_node_calls
            + grovedb_bulk_append_tree::amortized_compaction_hashes(chunk_power)
            + ROOT_AND_CONFIG_HASHES,
        sinsemilla_hash_calls: 0,
    }
}

/// Cost of the work a single `DenseTreeInsert` performs outside the parent
/// Merk (charged separately via `average/worst_case_merk_replace_tree`) on a
/// `DenseAppendOnlyFixedSizeTree` of `height`: the value write, the path
/// record write, and the buffer's fixed root-maintenance model (GROVE_V4),
/// which the insert bills in full (its append returns the whole cost). A
/// standalone dense tree's buffer IS its long-term storage, so its slot and
/// record are new storage (the record's key is the inserting position's,
/// written once; a catch-up of a buffer filled under GROVE_V1..V3 may
/// rewrite one, bounded by a second record as replaced).
///
/// `height` is the tree's declared height (`TreeType::DenseAppendOnlyFixedSizeTree(height)`
/// on the tree's own layer, average case) or [`PHYSICAL_MAX_CHUNK_POWER`]
/// (worst case, and when undeclared); clamped to the constructor's range.
#[cfg(feature = "minimal")]
pub(in crate::batch) fn dense_tree_insert_op_cost(value_size: u32, height: u8) -> OperationCost {
    let buffer = dense_buffer_model(height);
    let record_put = buffer.record_len.saturating_add(PER_PUT_OVERHEAD);
    OperationCost {
        // The stored element read by preprocessing, the value write, the
        // record write, and the model's record reads.
        seek_count: 3u32.saturating_add(buffer.cost.seek_count),
        storage_cost: StorageCost {
            // The value (key included) and the record: new storage.
            added_bytes: value_size
                .saturating_add(PER_PUT_OVERHEAD)
                .saturating_add(record_put),
            replaced_bytes: record_put,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        },
        // The stored element and the model's records.
        storage_loaded_bytes: CT_ELEMENT_LOAD_BASE as u64 + buffer.cost.storage_loaded_bytes,
        hash_node_calls: buffer.cost.hash_node_calls,
        sinsemilla_hash_calls: 0,
    }
}

/// The largest per-append share of the compaction overhead the type
/// permits — the smallest epoch (`chunk_power` 1) — for the worst-case
/// estimators, which have no declaration channel. The share shrinks as the
/// epoch grows, so the ceiling epoch is NOT the bound here. (The hash and
/// seek shares have the same property; see
/// `grovedb_bulk_append_tree::max_amortized_compaction_{hashes,seeks}`.)
#[cfg(feature = "minimal")]
pub(in crate::batch) fn max_amortized_compaction_added_bytes() -> u32 {
    grovedb_bulk_append_tree::amortized_compaction_added_bytes(2)
}

/// Estimated costs types
#[cfg(feature = "minimal")]
pub enum EstimatedCostsType {
    /// Average cast estimated costs type
    AverageCaseCostsType(HashMap<KeyInfoPath, EstimatedLayerInformation>),
    /// Worst case estimated costs type
    WorstCaseCostsType(HashMap<KeyInfoPath, WorstCaseLayerInformation>),
}
