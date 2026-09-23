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

use bincode::{Decode, DecodeUntrusted, Encode};
use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
use grovedb_merkle_mountain_range::MmrTreeProof;
#[cfg(feature = "storage")]
use grovedb_merkle_mountain_range::{MmrKeySize, MmrNode, MmrStore, MMR};
use grovedb_query::{Query, QueryItem};
#[cfg(feature = "storage")]
use grovedb_storage::StorageContext;

#[cfg(feature = "storage")]
use crate::BulkAppendTree;
use crate::{
    completed_chunk_entries, compute_state_root, error::BulkAppendError, leaf_count_to_mmr_size,
};

#[cfg(all(test, feature = "storage"))]
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
            QueryItem::AggregateCountOnRange(_) => {
                return Err(BulkAppendError::InvalidInput(
                    "AggregateCountOnRange is only supported on provable count trees, \
                     not on BulkAppendTree"
                        .into(),
                ));
            }
            QueryItem::AggregateSumOnRange(_) => {
                return Err(BulkAppendError::InvalidInput(
                    "AggregateSumOnRange is only supported on provable sum trees, \
                     not on BulkAppendTree"
                        .into(),
                ));
            }
            QueryItem::AggregateCountAndSumOnRange(_) => {
                return Err(BulkAppendError::InvalidInput(
                    "AggregateCountAndSumOnRange is only supported on \
                     ProvableCountProvableSumTree, not on BulkAppendTree"
                        .into(),
                ));
            }
        };
        ranges.push((start, end));
    }

    // Sort and merge overlapping / adjacent ranges
    Ok(normalize_ranges(&ranges))
}

/// Build the canonical [`Query`] selecting the position range
/// `[start, start + limit)`, with positions encoded as 8-byte big-endian
/// keys.
///
/// This is the query shape used by the paginated-scan pattern: prover and
/// verifier both derive it from `(start, limit)`, so a client only needs its
/// cursor and page size. `start + limit` saturates at `u64::MAX`, and
/// verification clamps the range to the tree's provable total count.
pub fn position_range_query(start: u64, limit: u16) -> Query {
    let end = start.saturating_add(limit as u64);
    Query {
        items: vec![QueryItem::Range(
            start.to_be_bytes().to_vec()..end.to_be_bytes().to_vec(),
        )],
        left_to_right: true,
        ..Query::default()
    }
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
#[derive(Debug, Clone, Encode, Decode, DecodeUntrusted)]
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
    /// * `query` - Query describing the positions to prove
    /// * `tree` - The BulkAppendTree to prove against
    #[cfg(feature = "storage")]
    pub fn generate<'db, S: StorageContext<'db>>(
        query: &Query,
        tree: &BulkAppendTree<S>,
    ) -> Result<Self, BulkAppendError> {
        let total_count = tree.total_count;
        let height = tree.height();
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
                    let last_chunk = std::cmp::min(range_end, chunk_boundary).saturating_sub(1)
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

            let mmr_store = MmrStore::with_key_size(&tree.dense_tree.storage, MmrKeySize::U32);
            let mmr = MMR::new_with_overlay(mmr_size, &mmr_store, tree.mmr_overlay.clone());
            let get_node = |pos: u64| -> grovedb_merkle_mountain_range::Result<Option<MmrNode>> {
                mmr.batch.element_at_position(pos).unwrap() // unwrap CostResult
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
                DenseTreeProof::generate(&tree.dense_tree, &[0])
                    .unwrap()
                    .map_err(|e| {
                        BulkAppendError::StorageError(format!(
                            "dense tree proof generation failed: {}",
                            e
                        ))
                    })?
            } else {
                let positions: Vec<u16> = buffer_positions.into_iter().collect();
                DenseTreeProof::generate(&tree.dense_tree, &positions)
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

    /// Generate a proof for the paginated position range
    /// `[start, start + limit)`.
    ///
    /// Convenience wrapper over [`generate`](Self::generate) using the
    /// canonical [`position_range_query`]. The proof is chunk-aligned: it
    /// carries each completed chunk blob overlapping the range plus the
    /// buffer entries in range, so proof size is O(chunks touched).
    ///
    /// Ranges past the end of the tree are valid and produce a proof of the
    /// (empty) result: absence of positions `>= total_count` falls out of
    /// the authenticated element's total count, not out of per-position
    /// absence proofs.
    #[cfg(feature = "storage")]
    pub fn generate_for_range<'db, S: StorageContext<'db>>(
        tree: &BulkAppendTree<S>,
        start: u64,
        limit: u16,
    ) -> Result<Self, BulkAppendError> {
        Self::generate(&position_range_query(start, limit), tree)
    }

    /// Verify this proof against the paginated position range
    /// `[start, start + limit)`.
    ///
    /// Convenience wrapper over
    /// [`verify_against_query`](Self::verify_against_query) using the
    /// canonical [`position_range_query`]. Returns the `(global_position,
    /// value)` pairs in the range, ascending and contiguous, clamped to
    /// `total_count`. Completeness is enforced: a proof missing any
    /// requested position below `total_count` is rejected. Positions
    /// `>= total_count` are provably absent by `total_count` itself, which
    /// callers must take from the authenticated BulkAppendTree element.
    pub fn verify_range(
        &self,
        expected_state_root: &[u8; 32],
        height: u8,
        total_count: u64,
        start: u64,
        limit: u16,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        self.verify_against_query(
            expected_state_root,
            height,
            total_count,
            &position_range_query(start, limit),
        )
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
        let completed_chunks = total_count / chunk_item_count;
        let dense_count = (total_count % chunk_item_count) as u16;
        let expected_mmr_size = leaf_count_to_mmr_size(completed_chunks);

        // Cross-validate MMR size: the proof's mmr_size must match the
        // expected size derived from (total_count, height).
        let proof_mmr_size = self.chunk_proof.mmr_size();
        if proof_mmr_size != expected_mmr_size {
            return Err(BulkAppendError::InvalidProof(format!(
                "chunk proof mmr_size {} does not match expected {} (total_count={}, \
                 completed_chunks={})",
                proof_mmr_size, expected_mmr_size, total_count, completed_chunks
            )));
        }

        // 1. Verify chunk MMR sub-proof
        let (mmr_root, chunk_blobs) = if self.chunk_proof.mmr_size() == 0 {
            // Empty MMR — no chunks exist
            ([0u8; 32], Vec::new())
        } else {
            let (root, verified_leaves) = self.chunk_proof.verify_and_get_root().map_err(|e| {
                BulkAppendError::InvalidProof(format!("chunk MMR proof verification failed: {}", e))
            })?;
            (root, verified_leaves)
        };

        // NOTE — intentionally NO "completed chunk length" check here.
        //
        // An audit (human or AI) may expect us to deserialize each completed
        // chunk blob and reject any that does not hold exactly
        // `chunk_item_count` entries (the concern raised in issue #688). That
        // check is cryptographically redundant and is deliberately omitted; do
        // not re-add it.
        //
        // A completed chunk's contents are fully bound to the root: the MMR
        // recomputes `leaf_hash(blob)` from the blob bytes, that leaf feeds
        // `mmr_root`, and `mmr_root` feeds `compute_state_root` below. So a
        // short, truncated, or otherwise tampered chunk blob yields a
        // *different* `computed_state_root`. Every caller binds that root to a
        // trusted value — `verify` / `verify_against_query` compare it against
        // `expected_state_root`, and the GroveDB lower-layer path folds it into
        // the parent Merk hash up to the consensus root — so a bad blob is
        // rejected by the root comparison, never reaching a length check. (The
        // GroveDB path additionally runs an independent completeness check that
        // rejects any queried position the proof omits.)
        //
        // Contrast with the `dense_count == 0` empty-buffer check below, which
        // *is* required: unused buffer data is NOT hashed into the root, so it
        // would be unbound and could be smuggled. Completed chunk bytes are
        // bound, so they cannot be.
        //
        // The only situation a length check would add anything is a caller that
        // trusts `total_count` yet verifies against an attacker-chosen root —
        // which is outside the trust model (such a caller can be handed an
        // entirely different, internally consistent tree). See PR #729 (closed).
        //
        // Value extraction (`BulkAppendTreeProofResult::values_in_ranges`) does
        // require each chunk blob it decodes to hold exactly `chunk_item_count`
        // entries. That check bounds resources; it is not the soundness check
        // above. The root comparison bounds nothing that happens before it,
        // and the GroveDB verifier extracts values before its caller compares
        // the returned root with a trusted one. Until then `height`,
        // `total_count` and every blob are attacker-chosen, and a 9-byte
        // fixed-format blob could otherwise declare 2^20 zero-sized entries.

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
            // When dense_count is 0, the buffer proof must be empty.
            // Reject proofs that smuggle data in an unused buffer.
            if !self.buffer_proof.entries.is_empty()
                || !self.buffer_proof.node_value_hashes.is_empty()
                || !self.buffer_proof.node_hashes.is_empty()
            {
                return Err(BulkAppendError::InvalidProof(
                    "buffer proof must be empty when dense_count is 0".to_string(),
                ));
            }
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
    /// Positions below `completed_chunks * chunk_item_count` live in chunk
    /// blobs (each chunk holds `chunk_item_count` items); positions at or
    /// above that boundary live in the dense buffer.
    ///
    /// Returns matched `(global_position, value)` pairs collected into `C`.
    /// `C` can be `Vec<(u64, Vec<u8>)>`, `BTreeMap<u64, Vec<u8>>`,
    /// `HashMap<u64, Vec<u8>>`, or any `FromIterator<(u64, Vec<u8>)>`.
    ///
    /// The pairs are yielded in the query's direction — ascending
    /// positions for `left_to_right`, descending otherwise — and
    /// `query.limit` (when set) caps the yield to the FIRST `limit`
    /// pairs of that ordering. So a descending limited query returns the
    /// highest matched positions, exactly as the GroveDB verifier and a
    /// direct tree read would. Completeness is still checked against the
    /// full query ranges before the cap applies: the proof must carry
    /// every matched position, not merely the window it yields.
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

        // Always verify the proof cryptographically, even for empty queries.
        let (computed_state_root, result) = self.verify_and_compute_root(height, total_count)?;

        if &computed_state_root != expected_state_root {
            return Err(BulkAppendError::InvalidProof(format!(
                "state root mismatch: expected {}, computed {}",
                hex::encode(expected_state_root),
                hex::encode(computed_state_root)
            )));
        }

        if ranges.is_empty() {
            return Ok(std::iter::empty().collect());
        }

        let chunk_item_count = ((1u32 << height) - 1) as u64 + 1;
        let completed_chunks = total_count / chunk_item_count;
        let buffer_start = completed_chunks * chunk_item_count;

        // ── Check chunk completeness ───────────────────────────────────
        let proved_chunks: BTreeSet<u64> = result.chunk_blobs.iter().map(|(idx, _)| *idx).collect();

        for &(range_start, range_end) in &ranges {
            if range_start < buffer_start {
                let first = range_start / chunk_item_count;
                let last =
                    std::cmp::min(range_end, buffer_start).saturating_sub(1) / chunk_item_count;
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
        // In the trusted query direction, capped there, so a descending
        // limit keeps the highest positions rather than the lowest ones.
        let values = result.values_in_ranges_directed(
            &ranges,
            query.left_to_right,
            query.limit.map(usize::from),
        )?;
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
        let (proof, consumed): (Self, usize) = bincode::decode_from_slice_untrusted(bytes, config)
            .map_err(|e| {
                BulkAppendError::CorruptedData(format!(
                    "failed to decode BulkAppendTreeProof: {}",
                    e
                ))
            })?;
        if consumed != bytes.len() {
            return Err(BulkAppendError::CorruptedData(format!(
                "BulkAppendTreeProof decode did not consume all bytes: consumed {}, total {}",
                consumed,
                bytes.len()
            )));
        }
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
    /// the specified range. See [`values_in_ranges`](Self::values_in_ranges).
    pub fn values_in_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        self.values_in_ranges(&[(start, end)])
    }

    /// Extract the values at every position covered by `ranges`, ascending
    /// by position. See [`values_in_ranges_directed`](Self::values_in_ranges_directed).
    pub fn values_in_ranges(
        &self,
        ranges: &[(u64, u64)],
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        self.values_in_ranges_directed(ranges, true, None)
    }

    /// Extract the values at the positions covered by `ranges`: ascending
    /// when `left_to_right`, descending otherwise, and at most `limit` of
    /// them, taken from that end.
    ///
    /// Each range is half-open `[start, end)`. They may come in any order and
    /// may overlap or be empty.
    ///
    /// The work is bounded by the returned values, never by `total_count`
    /// and `height` alone. A caller may reach this before the proof's root is
    /// bound to a trusted value: the GroveDB verifier's root is authenticated
    /// only when its caller compares it with a trusted root, after
    /// verification returns. So `total_count`, `height` and every blob can
    /// still be attacker-chosen here, and:
    ///
    /// - the result's shape is checked first (see
    ///   [`proved_position_spans`](Self::proved_position_spans)), so no
    ///   position is produced twice and no chunk reaches into the buffer;
    /// - chunks are visited in the query's direction and extraction stops
    ///   once `limit` values are produced;
    /// - a chunk is decoded only for the entries it contributes, and must
    ///   hold exactly `2^height` entries, checked without allocating per
    ///   entry (see [`completed_chunk_entries`]).
    pub fn values_in_ranges_directed(
        &self,
        ranges: &[(u64, u64)],
        left_to_right: bool,
        limit: Option<usize>,
    ) -> Result<Vec<(u64, Vec<u8>)>, BulkAppendError> {
        let (chunk_item_count, buffer_start) = self.checked_layout()?;
        let ranges = normalize_ranges(ranges);
        let mut remaining = limit.unwrap_or(usize::MAX);
        let mut result = Vec::new();

        let mut buffer: Vec<(u64, &Vec<u8>)> = self
            .dense_entries
            .iter()
            .map(|(pos, value)| (buffer_start + *pos as u64, value))
            .filter(|(global_pos, _)| in_ranges(*global_pos, &ranges))
            .collect();
        buffer.sort_unstable_by_key(|(global_pos, _)| *global_pos);

        // Chunk positions all lie below `buffer_start`, buffer positions at
        // or above it.
        if left_to_right {
            for (chunk_idx, blob) in &self.chunk_blobs {
                if remaining == 0 {
                    break;
                }
                extract_chunk(
                    *chunk_idx,
                    blob,
                    chunk_item_count,
                    &ranges,
                    true,
                    &mut remaining,
                    &mut result,
                )?;
            }
            take_buffer(buffer.into_iter(), &mut remaining, &mut result);
        } else {
            take_buffer(buffer.into_iter().rev(), &mut remaining, &mut result);
            for (chunk_idx, blob) in self.chunk_blobs.iter().rev() {
                if remaining == 0 {
                    break;
                }
                extract_chunk(
                    *chunk_idx,
                    blob,
                    chunk_item_count,
                    &ranges,
                    false,
                    &mut remaining,
                    &mut result,
                )?;
            }
        }
        Ok(result)
    }

    /// The positions this proof carries, as sorted, disjoint, half-open
    /// spans, computed without decoding any chunk blob: each chunk covers
    /// its `2^height` slot and each buffer entry its own position.
    ///
    /// A completeness check against these spans costs nothing per position,
    /// so it can run before any extraction. A chunk counted here but never
    /// decoded contributes no value; its bytes are still bound by the MMR
    /// leaf hash.
    ///
    /// Also checks the result's shape: chunk blobs must come in strictly
    /// ascending chunk-index order, each index below
    /// `total_count / 2^height`, and buffer entries must be distinct
    /// positions below `total_count`.
    pub fn proved_position_spans(&self) -> Result<Vec<(u64, u64)>, BulkAppendError> {
        let (chunk_item_count, buffer_start) = self.checked_layout()?;
        let chunk_spans = self.chunk_blobs.iter().map(|(chunk_idx, _)| {
            let chunk_start = chunk_idx * chunk_item_count;
            (chunk_start, chunk_start + chunk_item_count)
        });
        let buffer_spans = self.dense_entries.iter().map(|(pos, _)| {
            let global_pos = buffer_start + *pos as u64;
            (global_pos, global_pos + 1)
        });
        Ok(normalize_ranges(
            &chunk_spans.chain(buffer_spans).collect::<Vec<_>>(),
        ))
    }

    /// Check the result's shape (see
    /// [`proved_position_spans`](Self::proved_position_spans)) and return
    /// `(2^height, buffer_start)`. Every chunk span and buffer position then
    /// fits below `total_count`, so the callers' position arithmetic cannot
    /// overflow.
    fn checked_layout(&self) -> Result<(u64, u64), BulkAppendError> {
        if self.height == 0 || self.height > 16 {
            return Err(BulkAppendError::InvalidProof(format!(
                "invalid height {} in proof result (must be 1..=16)",
                self.height
            )));
        }
        let chunk_item_count = 1u64 << self.height;
        let completed_chunks = self.total_count / chunk_item_count;
        // At most `total_count`, so it cannot overflow.
        let buffer_start = completed_chunks * chunk_item_count;

        let mut previous_chunk: Option<u64> = None;
        for &(chunk_idx, _) in &self.chunk_blobs {
            if let Some(previous) = previous_chunk
                && chunk_idx <= previous
            {
                return Err(BulkAppendError::InvalidProof(format!(
                    "chunk blobs must be in strictly ascending chunk-index order: chunk {} \
                     follows chunk {}",
                    chunk_idx, previous
                )));
            }
            previous_chunk = Some(chunk_idx);
            // `chunk_idx < completed_chunks` keeps the chunk's end at or
            // below `buffer_start`.
            if chunk_idx >= completed_chunks {
                return Err(BulkAppendError::InvalidProof(format!(
                    "chunk blob {} is at or beyond the buffer start (completed chunks: {})",
                    chunk_idx, completed_chunks
                )));
            }
        }

        let mut buffer_positions = BTreeSet::new();
        for &(pos, _) in &self.dense_entries {
            if (pos as u64) >= self.total_count - buffer_start {
                return Err(BulkAppendError::InvalidProof(format!(
                    "buffer entry {} is beyond total_count {}",
                    pos, self.total_count
                )));
            }
            if !buffer_positions.insert(pos) {
                return Err(BulkAppendError::InvalidProof(format!(
                    "buffer entry {} appears twice",
                    pos
                )));
            }
        }
        Ok((chunk_item_count, buffer_start))
    }
}

/// Append the values of chunk `chunk_idx` at the positions `ranges` covers,
/// in the given direction and at most `*remaining` of them, decoding only
/// those entries. The chunk's index was checked by `checked_layout`.
fn extract_chunk(
    chunk_idx: u64,
    blob: &[u8],
    chunk_item_count: u64,
    ranges: &[(u64, u64)],
    left_to_right: bool,
    remaining: &mut usize,
    out: &mut Vec<(u64, Vec<u8>)>,
) -> Result<(), BulkAppendError> {
    let chunk_start = chunk_idx * chunk_item_count;
    let chunk_end = chunk_start + chunk_item_count;
    // The chunk-local index ranges the query touches, ascending.
    let first = ranges.partition_point(|&(_, end)| end <= chunk_start);
    let mut wanted: Vec<(usize, usize)> = ranges[first..]
        .iter()
        .take_while(|&&(start, _)| start < chunk_end)
        .map(|&(start, end)| {
            (
                (start.max(chunk_start) - chunk_start) as usize,
                (end.min(chunk_end) - chunk_start) as usize,
            )
        })
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    keep_first_positions(&mut wanted, *remaining, left_to_right);

    let entries = completed_chunk_entries(blob, chunk_item_count, &wanted).map_err(|e| {
        BulkAppendError::InvalidProof(format!(
            "failed to deserialize chunk blob {}: {}",
            chunk_idx, e
        ))
    })?;
    *remaining -= entries.len();
    let rows = wanted
        .iter()
        .flat_map(|&(start, end)| start..end)
        .map(|i| chunk_start + i as u64)
        .zip(entries);
    if left_to_right {
        out.extend(rows);
    } else {
        let rows: Vec<_> = rows.collect();
        out.extend(rows.into_iter().rev());
    }
    Ok(())
}

/// Append buffer `entries` to `out` until `*remaining` runs out.
fn take_buffer<'a>(
    entries: impl Iterator<Item = (u64, &'a Vec<u8>)>,
    remaining: &mut usize,
    out: &mut Vec<(u64, Vec<u8>)>,
) {
    for (global_pos, value) in entries.take(*remaining) {
        out.push((global_pos, value.clone()));
        *remaining -= 1;
    }
}

/// Trim sorted, disjoint index ranges to their first `budget` indices, from
/// the low end when `from_low`, from the high end otherwise.
fn keep_first_positions(ranges: &mut Vec<(usize, usize)>, mut budget: usize, from_low: bool) {
    if !from_low {
        ranges.reverse();
    }
    let mut kept = 0;
    for range in ranges.iter_mut() {
        if budget == 0 {
            break;
        }
        let take = (range.1 - range.0).min(budget);
        *range = if from_low {
            (range.0, range.0 + take)
        } else {
            (range.1 - take, range.1)
        };
        budget -= take;
        kept += 1;
    }
    ranges.truncate(kept);
    if !from_low {
        ranges.reverse();
    }
}

/// Sort `ranges`, drop the empty ones and merge the overlapping or adjacent
/// ones, giving the sorted, disjoint form [`in_ranges`] expects.
fn normalize_ranges(ranges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut sorted: Vec<(u64, u64)> = ranges.iter().copied().filter(|(s, e)| s < e).collect();
    sorted.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}
