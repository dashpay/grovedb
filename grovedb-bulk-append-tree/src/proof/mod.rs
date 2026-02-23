//! BulkAppendTree proof generation and verification.
//!
//! A `BulkAppendTreeProof` contains two sub-proofs matching the two sub-trees:
//! - A [`ChunkProof`] for the chunk MMR (completed epochs)
//! - A [`DenseTreeProof`] for the buffer (dense fixed-sized Merkle tree)
//!
//! The proof ties into the GroveDB hierarchy: the parent Merk proves the
//! BulkAppendTree element bytes (containing the state_root), and this proof
//! shows that queried data is consistent with that root.

use std::collections::BTreeSet;

use bincode::{Decode, Encode};
use grovedb_dense_fixed_sized_merkle_tree::{DenseTreeProof, DenseTreeStore};
use grovedb_merkle_mountain_range::{MmrNode, MmrTreeProof};
use grovedb_query::{Query, QueryItem};

use grovedb_merkle_mountain_range::MMRStoreReadOps;

use crate::{
    compute_state_root, deserialize_chunk_blob, error::BulkAppendError,
    leaf_count_to_mmr_size,
};

mod tests;
// ── Query → global position helpers ────────────────────────────────────

/// Decode big-endian bytes as a u64 global position (1–8 bytes).
fn bytes_to_global_position(bytes: &[u8]) -> Result<u64, BulkAppendError> {
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(BulkAppendError::InvalidInput(format!(
            "position byte length must be 1–8, got {}",
            bytes.len()
        )));
    }
    let mut buf = [0u8; 8];
    buf[8 - bytes.len()..].copy_from_slice(bytes);
    Ok(u64::from_be_bytes(buf))
}

/// Resolve a [`Query`] into sorted, merged, non-overlapping `[start, end)`
/// ranges clamped to `[0, total_count)`.
fn query_to_ranges(query: &Query, total_count: u64) -> Result<Vec<(u64, u64)>, BulkAppendError> {
    if query.has_subquery() {
        return Err(BulkAppendError::InvalidInput(
            "subqueries are not supported for BulkAppendTree queries".into(),
        ));
    }

    let mut ranges = Vec::new();

    for item in &query.items {
        let (start, end) = match item {
            QueryItem::Key(k) => {
                let pos = bytes_to_global_position(k)?;
                if pos < total_count {
                    (pos, pos + 1)
                } else {
                    continue;
                }
            }
            QueryItem::Range(r) => {
                let s = bytes_to_global_position(&r.start)?;
                let e = bytes_to_global_position(&r.end)?.min(total_count);
                if s >= e {
                    continue;
                }
                (s, e)
            }
            QueryItem::RangeInclusive(r) => {
                let s = bytes_to_global_position(r.start())?;
                let e = bytes_to_global_position(r.end())?
                    .saturating_add(1)
                    .min(total_count);
                if s >= e {
                    continue;
                }
                (s, e)
            }
            QueryItem::RangeFull(..) => {
                if total_count == 0 {
                    continue;
                }
                (0, total_count)
            }
            QueryItem::RangeFrom(r) => {
                let s = bytes_to_global_position(&r.start)?;
                if s >= total_count {
                    continue;
                }
                (s, total_count)
            }
            QueryItem::RangeTo(r) => {
                let e = bytes_to_global_position(&r.end)?.min(total_count);
                if e == 0 {
                    continue;
                }
                (0, e)
            }
            QueryItem::RangeToInclusive(r) => {
                let e = bytes_to_global_position(&r.end)?
                    .saturating_add(1)
                    .min(total_count);
                if e == 0 {
                    continue;
                }
                (0, e)
            }
            QueryItem::RangeAfter(r) => {
                let s = bytes_to_global_position(&r.start)?.saturating_add(1);
                if s >= total_count {
                    continue;
                }
                (s, total_count)
            }
            QueryItem::RangeAfterTo(r) => {
                let s = bytes_to_global_position(&r.start)?.saturating_add(1);
                let e = bytes_to_global_position(&r.end)?.min(total_count);
                if s >= e {
                    continue;
                }
                (s, e)
            }
            QueryItem::RangeAfterToInclusive(r) => {
                let s = bytes_to_global_position(r.start())?.saturating_add(1);
                let e = bytes_to_global_position(r.end())?
                    .saturating_add(1)
                    .min(total_count);
                if s >= e {
                    continue;
                }
                (s, e)
            }
        };
        ranges.push((start, end));
    }

    // Sort and merge overlapping / adjacent ranges
    ranges.sort();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (s, e) in ranges {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }

    Ok(merged)
}

/// Check whether `pos` falls inside any of the sorted, non-overlapping ranges.
fn in_ranges(pos: u64, ranges: &[(u64, u64)]) -> bool {
    ranges
        .binary_search_by(|&(s, e)| {
            if pos < s {
                std::cmp::Ordering::Greater
            } else if pos >= e {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// A proof that specific data exists in a BulkAppendTree.
///
/// Contains two sub-proofs — one for each sub-tree:
/// - [`MmrTreeProof`] for the chunk MMR (completed epochs)
/// - [`DenseTreeProof`] for the buffer (dense fixed-sized Merkle tree)
///
/// Verification recomputes `state_root = blake3("bulk_state" || mmr_root ||
/// dense_tree_root)` from the two sub-proofs and checks it against the
/// expected root.
#[derive(Debug, Clone, Encode, Decode)]
pub struct BulkAppendTreeProof {
    /// Sub-proof for the chunk MMR (completed epochs).
    /// When no chunks exist, this is an empty proof with `mmr_size = 0`.
    pub chunk_proof: MmrTreeProof,
    /// Sub-proof for the buffer (dense fixed-sized Merkle tree).
    pub buffer_proof: DenseTreeProof,
}

impl BulkAppendTreeProof {
    /// Generate a BulkAppendTree proof for a [`Query`].
    ///
    /// Query items encode global u64 positions as big-endian bytes (1–8 bytes).
    ///
    /// # Arguments
    /// * `total_count` - Total values ever appended across all chunks and the
    ///   current buffer: `completed_chunks * chunk_item_count + buffer_count`
    /// * `height` - Dense tree height (1–16); chunk_item_count = 2^height
    /// * `query` - Query describing the positions to prove
    /// * `dense_store` - Store for dense tree buffer values
    /// * `mmr_store` - Store for MMR epoch data
    pub fn generate<D: DenseTreeStore, M>(
        total_count: u64,
        height: u8,
        query: &Query,
        dense_store: &D,
        mmr_store: &M,
    ) -> Result<Self, BulkAppendError>
    where
        for<'a> &'a M: MMRStoreReadOps,
    {
        let chunk_item_count = ((1u32 << height) - 1) as u64 + 1; // capacity + 1 = 2^height
        let completed_chunks = total_count / chunk_item_count;
        let dense_count = (total_count % chunk_item_count) as u16;
        let mmr_size = leaf_count_to_mmr_size(completed_chunks);

        let ranges = query_to_ranges(query, total_count)?;

        // ── Chunk sub-proof (MMR) ────────────────────────────────────

        let chunk_proof = if completed_chunks == 0 {
            // Empty MMR — no chunks exist
            MmrTreeProof::new(0, Vec::new(), Vec::new())
        } else {
            // Determine overlapping chunk indices from all query ranges
            let mut chunk_indices_set = BTreeSet::new();
            let chunk_boundary = completed_chunks * chunk_item_count;
            for &(range_start, range_end) in &ranges {
                if range_start < chunk_boundary {
                    let first_chunk = range_start / chunk_item_count;
                    let last_chunk = std::cmp::min(range_end, chunk_boundary)
                        .saturating_sub(1)
                        / chunk_item_count;
                    for idx in first_chunk..=last_chunk {
                        chunk_indices_set.insert(idx);
                    }
                }
            }

            // When no chunks overlap the query, prove chunk 0 to anchor the
            // MMR root.
            if chunk_indices_set.is_empty() {
                chunk_indices_set.insert(0);
            }

            let chunk_indices: Vec<u64> = chunk_indices_set.into_iter().collect();

            let get_node = |pos: u64| -> grovedb_merkle_mountain_range::Result<Option<MmrNode>> {
                mmr_store
                    .element_at_position(pos)
                    .unwrap() // unwrap CostResult
            };

            MmrTreeProof::generate(mmr_size, &chunk_indices, get_node).map_err(|e| {
                BulkAppendError::MmrError(format!("MmrTreeProof::generate failed: {}", e))
            })?
        };

        // ── Buffer sub-proof (dense tree) ────────────────────────────

        let buffer_proof = if dense_count > 0 {
            let buffer_start = completed_chunks * chunk_item_count;
            let buffer_end = buffer_start + dense_count as u64;

            // Collect buffer-local positions from all query ranges
            let mut buffer_positions = BTreeSet::new();
            for &(range_start, range_end) in &ranges {
                let overlap_start = range_start.max(buffer_start);
                let overlap_end = range_end.min(buffer_end);
                if overlap_start < overlap_end {
                    for pos in overlap_start..overlap_end {
                        buffer_positions.insert((pos - buffer_start) as u16);
                    }
                }
            }

            if buffer_positions.is_empty() {
                // No buffer positions in query — generate a proof for position
                // 0 anyway so we can verify the root hash.
                DenseTreeProof::generate(height, dense_count, &[0], dense_store)
                    .unwrap()
                    .map_err(|e| {
                        BulkAppendError::StorageError(format!(
                            "dense tree proof generation failed: {}",
                            e
                        ))
                    })?
            } else {
                let positions: Vec<u16> = buffer_positions.into_iter().collect();
                DenseTreeProof::generate(height, dense_count, &positions, dense_store)
                    .unwrap()
                    .map_err(|e| {
                        BulkAppendError::StorageError(format!(
                            "dense tree proof generation failed: {}",
                            e
                        ))
                    })?
            }
        } else {
            // Empty dense tree — empty proof
            DenseTreeProof {
                entries: Vec::new(),
                node_value_hashes: Vec::new(),
                node_hashes: Vec::new(),
            }
        };

        Ok(BulkAppendTreeProof {
            chunk_proof,
            buffer_proof,
        })
    }

    /// Verify this proof against an expected state root.
    ///
    /// `height` and `total_count` come from the authenticated BulkAppendTree
    /// element — they are not duplicated in the proof.
    ///
    /// This is a pure function — no database access needed.
    pub fn verify(
        &self,
        expected_state_root: &[u8; 32],
        height: u8,
        total_count: u64,
    ) -> Result<BulkAppendTreeProofResult, BulkAppendError> {
        let (computed_state_root, result) = self.verify_and_compute_root(height, total_count)?;

        if &computed_state_root != expected_state_root {
            return Err(BulkAppendError::InvalidProof(format!(
                "state root mismatch: expected {}, computed {}",
                hex::encode(expected_state_root),
                hex::encode(computed_state_root)
            )));
        }

        Ok(result)
    }

    /// Verify this proof's internal consistency and return the computed
    /// state root.
    pub fn verify_and_compute_root(
        &self,
        height: u8,
        total_count: u64,
    ) -> Result<([u8; 32], BulkAppendTreeProofResult), BulkAppendError> {
        // 0. Validate height
        if !(1..=16).contains(&height) {
            return Err(BulkAppendError::InvalidProof(format!(
                "invalid height: {} (must be 1..=16)",
                height
            )));
        }

        let chunk_item_count = ((1u32 << height) - 1) as u64 + 1;
        let dense_count = (total_count % chunk_item_count) as u16;

        // 1. Verify chunk MMR sub-proof
        let (mmr_root, chunk_blobs) = if self.chunk_proof.mmr_size() == 0 {
            // Empty MMR — no chunks exist
            ([0u8; 32], Vec::new())
        } else {
            let (root, verified_leaves) =
                self.chunk_proof.verify_and_get_root().map_err(|e| {
                    BulkAppendError::InvalidProof(format!(
                        "chunk MMR proof verification failed: {}",
                        e
                    ))
                })?;
            (root, verified_leaves)
        };

        // 2. Verify dense tree buffer sub-proof
        let (dense_root, dense_entries) = if dense_count > 0 {
            self.buffer_proof
                .verify_and_get_root(height, dense_count)
                .map_err(|e| {
                    BulkAppendError::InvalidProof(format!(
                        "dense tree proof verification failed: {}",
                        e
                    ))
                })?
        } else {
            ([0u8; 32], Vec::new())
        };

        // 3. Compute state_root from the two sub-tree roots
        let computed_state_root = compute_state_root(&mmr_root, &dense_root);

        Ok((
            computed_state_root,
            BulkAppendTreeProofResult {
                chunk_blobs,
                dense_entries,
                total_count,
                height,
            },
        ))
    }

    /// Verify this proof against a [`Query`].
    ///
    /// Combines cryptographic verification with completeness checking.
    /// Query items encode global u64 positions as big-endian bytes (1–8 bytes).
    ///
    /// Positions below `completed_chunks * chunk_item_count` live in chunk blobs
    /// (each chunk holds `chunk_item_count` items); positions at or above that
    /// boundary live in the dense buffer.
    ///
    /// Returns matched `(global_position, value)` pairs collected into `C`.
    /// `C` can be `Vec<(u64, Vec<u8>)>`, `BTreeMap<u64, Vec<u8>>`,
    /// `HashMap<u64, Vec<u8>>`, or any `FromIterator<(u64, Vec<u8>)>`.
    pub fn verify_against_query<C>(
        &self,
        expected_state_root: &[u8; 32],
        height: u8,
        total_count: u64,
        query: &Query,
    ) -> Result<C, BulkAppendError>
    where
        C: FromIterator<(u64, Vec<u8>)>,
    {
        let ranges = query_to_ranges(query, total_count)?;
        if ranges.is_empty() {
            return Ok(std::iter::empty().collect());
        }

        let (computed_state_root, result) = self.verify_and_compute_root(height, total_count)?;

        if &computed_state_root != expected_state_root {
            return Err(BulkAppendError::InvalidProof(format!(
                "state root mismatch: expected {}, computed {}",
                hex::encode(expected_state_root),
                hex::encode(computed_state_root)
            )));
        }

        let chunk_item_count = ((1u32 << height) - 1) as u64 + 1;
        let completed_chunks = total_count / chunk_item_count;
        let buffer_start = completed_chunks * chunk_item_count;

        // ── Check chunk completeness ───────────────────────────────────
        let proved_chunks: BTreeSet<u64> =
            result.chunk_blobs.iter().map(|(idx, _)| *idx).collect();

        for &(range_start, range_end) in &ranges {
            if range_start < buffer_start {
                let first = range_start / chunk_item_count;
                let last = std::cmp::min(range_end, buffer_start)
                    .saturating_sub(1)
                    / chunk_item_count;
                for idx in first..=last {
                    if !proved_chunks.contains(&idx) {
                        return Err(BulkAppendError::InvalidProof(format!(
                            "proof missing chunk {} required by query",
                            idx
                        )));
                    }
                }
            }
        }

        // ── Check buffer completeness ──────────────────────────────────
        let proved_positions: BTreeSet<u16> =
            result.dense_entries.iter().map(|(pos, _)| *pos).collect();

        for &(range_start, range_end) in &ranges {
            if range_end > buffer_start {
                let buf_s = range_start.saturating_sub(buffer_start) as u16;
                let buf_e = (range_end - buffer_start) as u16;
                for pos in buf_s..buf_e {
                    if !proved_positions.contains(&pos) {
                        return Err(BulkAppendError::InvalidProof(format!(
                            "proof missing buffer position {} (global {}) required by query",
                            pos,
                            buffer_start + pos as u64
                        )));
                    }
                }
            }
        }

        // ── Extract values matching the query ──────────────────────────
        let mut values = Vec::new();

        for (chunk_idx, blob) in &result.chunk_blobs {
            let entries = deserialize_chunk_blob(blob).map_err(|e| {
                BulkAppendError::CorruptedData(format!(
                    "failed to deserialize chunk blob {}: {}",
                    chunk_idx, e
                ))
            })?;
            let chunk_start = chunk_idx * chunk_item_count;
            for (i, value) in entries.into_iter().enumerate() {
                let global_pos = chunk_start + i as u64;
                if in_ranges(global_pos, &ranges) {
                    values.push((global_pos, value));
                }
            }
        }

        for (pos, value) in &result.dense_entries {
            let global_pos = buffer_start + *pos as u64;
            if in_ranges(global_pos, &ranges) {
                values.push((global_pos, value.clone()));
            }
        }

        values.sort_by_key(|(pos, _)| *pos);
        Ok(values.into_iter().collect())
    }

    /// Serialize this proof to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, BulkAppendError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config).map_err(|e| {
            BulkAppendError::CorruptedData(format!("failed to encode BulkAppendTreeProof: {}", e))
        })
    }

    /// Deserialize a proof from bytes.
    ///
    /// The bincode size limit is capped at 100 MB to prevent crafted length
    /// headers from causing huge allocations.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BulkAppendError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>();
        let (proof, _) = bincode::decode_from_slice(bytes, config).map_err(|e| {
            BulkAppendError::CorruptedData(format!("failed to decode BulkAppendTreeProof: {}", e))
        })?;
        Ok(proof)
    }
}

/// Result of a verified BulkAppendTree proof.
#[derive(Debug, Clone)]
pub struct BulkAppendTreeProofResult {
    /// Chunk blobs overlapping the queried range (chunk_index, blob_bytes).
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,
    /// Dense tree entries proved (position in dense tree, value).
    pub dense_entries: Vec<(u16, Vec<u8>)>,
    /// Total count of values in the tree.
    pub total_count: u64,
    /// Dense tree height.
    pub height: u8,
}

impl BulkAppendTreeProofResult {
    /// Extract values in the position range [start, end).
    ///
    /// Collects values from chunk blobs and dense tree entries that fall within
    /// the specified range.
    pub fn values_in_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        let chunk_item_count = ((1u32 << self.height) - 1) as u64 + 1;
        let completed_chunks = self.total_count / chunk_item_count;
        let buffer_start = completed_chunks * chunk_item_count;
        let mut result = Vec::new();

        // Extract from chunk blobs
        for (chunk_idx, blob) in &self.chunk_blobs {
            let values = deserialize_chunk_blob(blob).map_err(|e| {
                BulkAppendError::CorruptedData(format!(
                    "failed to deserialize chunk blob {}: {}",
                    chunk_idx, e
                ))
            })?;

            let chunk_start = chunk_idx * chunk_item_count;
            for (i, value) in values.into_iter().enumerate() {
                let global_pos = chunk_start + i as u64;
                if global_pos >= start && global_pos < end {
                    result.push((global_pos, value));
                }
            }
        }

        // Extract from dense tree entries
        for (pos, value) in &self.dense_entries {
            let global_pos = buffer_start + *pos as u64;
            if global_pos >= start && global_pos < end {
                result.push((global_pos, value.clone()));
            }
        }

        result.sort_by_key(|(pos, _)| *pos);
        Ok(result)
    }
}

