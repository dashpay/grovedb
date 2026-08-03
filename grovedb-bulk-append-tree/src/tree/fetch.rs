//! Read operations for BulkAppendTree.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
use grovedb_merkle_mountain_range::{leaf_to_pos, MmrKeySize, MmrStore, MMR};
use grovedb_query::Query;
use grovedb_storage::StorageContext;

use super::{BulkAppendTree, RangePage};
use crate::{chunk::deserialize_chunk_blob, BulkAppendError};

/// Result of querying the dense tree buffer.
#[derive(Debug, Clone)]
pub struct BufferQueryResult {
    /// The `(position, value)` pairs matching the query.
    pub entries: Vec<(u16, Vec<u8>)>,
    /// The dense tree inclusion proof for the queried positions.
    pub proof: DenseTreeProof,
}

/// Result of querying completed chunks from the chunk MMR.
#[derive(Debug, Clone)]
pub struct ChunkQueryResult {
    /// The `(chunk_index, deserialized_entries)` for each queried chunk.
    pub chunks: Vec<(u64, Vec<Vec<u8>>)>,
    /// MMR proof sibling/peak hashes binding the chunks to the MMR root.
    pub mmr_proof_items: Vec<[u8; 32]>,
    /// The chunk MMR root hash.
    pub mmr_root: [u8; 32],
}

impl<'db, S: StorageContext<'db>> BulkAppendTree<S> {
    // ── Buffer operations (dense fixed-sized Merkle tree) ────────────

    /// Get a single value from the dense tree buffer by its buffer-local
    /// position.
    ///
    /// This reads from the **buffer** (dense fixed-sized Merkle tree), not
    /// from completed chunks. The position is relative to the current buffer
    /// cycle (0-based).
    pub fn get_buffer_value(&self, position: u16) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if position >= self.buffer_count() {
            return Ok(None);
        }
        self.dense_tree.get(position).unwrap().map_err(|e| {
            BulkAppendError::StorageError(format!("dense tree get at {} failed: {}", position, e))
        })
    }

    /// Query the buffer using a dense tree query.
    ///
    /// This queries the **buffer** (dense fixed-sized Merkle tree) which holds
    /// values that haven't been compacted into a chunk yet.
    ///
    /// Returns a [`BufferQueryResult`] containing the matched `(position,
    /// value)` pairs and the dense tree inclusion proof.
    pub fn query_buffer(&self, query: &Query) -> Result<BufferQueryResult, BulkAppendError> {
        let proof = DenseTreeProof::generate_for_query(&self.dense_tree, query)
            .unwrap()
            .map_err(|e| {
                BulkAppendError::StorageError(format!("dense tree query failed: {}", e))
            })?;
        let entries = proof.entries.clone();
        Ok(BufferQueryResult { entries, proof })
    }

    // ── Range operations (chunks + buffer) ───────────────────────────

    /// Fetch entries for the position range `[start, start + limit)`,
    /// clamped to the tree's total count.
    ///
    /// This is the paginated-scan read path: clients walking "all entries
    /// since my cursor" call it with their cursor as `start` and advance by
    /// `entries.len()`. The read is chunk-aligned — each completed chunk
    /// overlapping the range is read and deserialized exactly once, so a
    /// page costs O(chunks touched) blob reads plus one read per buffer
    /// entry, not O(entries) random reads.
    ///
    /// Absence needs no lookup: positions `>= total_count` do not exist, so
    /// a page shorter than `limit` means the end of the tree was reached.
    ///
    /// Returns a [`CostResult`] so callers can charge the page's actual
    /// storage work (chunk MMR seeks and buffer reads) against cost limits.
    pub fn get_range(&self, start: u64, limit: u16) -> CostResult<RangePage, BulkAppendError> {
        let mut cost = OperationCost::default();

        let total_count = self.total_count;
        let end = start.saturating_add(limit as u64).min(total_count);
        if start >= end {
            return Ok(RangePage {
                entries: Vec::new(),
                total_count,
            })
            .wrap_with_cost(cost);
        }
        let mut entries = Vec::with_capacity((end - start) as usize);

        let epoch_size = self.epoch_size();
        let buffer_start = self.chunk_count() * epoch_size;

        // Completed chunks overlapping [start, min(end, buffer_start)).
        // The MMR (with its overlay clone) is built once and reused for every
        // chunk in the page — going through `get_chunk_value` would rebuild
        // it, and re-clone the overlay, per chunk.
        let chunk_end = end.min(buffer_start);
        if start < chunk_end {
            let first_chunk = start / epoch_size;
            let last_chunk = (chunk_end - 1) / epoch_size;
            let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
            let mmr = MMR::new_with_overlay(self.mmr_size(), &mmr_store, self.mmr_overlay.clone());
            for chunk_idx in first_chunk..=last_chunk {
                let node = match mmr
                    .batch
                    .element_at_position(leaf_to_pos(chunk_idx))
                    .unwrap_add_cost(&mut cost)
                {
                    Ok(node) => node,
                    Err(e) => {
                        return Err(BulkAppendError::MmrError(format!(
                            "failed to read MMR node for chunk {}: {}",
                            chunk_idx, e
                        )))
                        .wrap_with_cost(cost);
                    }
                };
                let Some(blob) = node.and_then(|n| n.into_value()) else {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "missing chunk blob for index {}",
                        chunk_idx
                    )))
                    .wrap_with_cost(cost);
                };
                let chunk_entries = match deserialize_chunk_blob(&blob) {
                    Ok(chunk_entries) => chunk_entries,
                    Err(e) => return Err(e).wrap_with_cost(cost),
                };
                // A completed chunk holds exactly `epoch_size` entries — a
                // short blob would silently omit positions and an oversized
                // one would overlap the next chunk, breaking the contiguous
                // page contract. Unlike proof verification (where chunk
                // bytes are bound to the state root and a length check is
                // redundant — see the NOTE in proof/mod.rs), this raw read
                // path has no root comparison backing it, so the length is
                // validated here.
                if chunk_entries.len() as u64 != epoch_size {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "chunk {} holds {} entries, expected {}",
                        chunk_idx,
                        chunk_entries.len(),
                        epoch_size
                    )))
                    .wrap_with_cost(cost);
                }
                let chunk_start = chunk_idx * epoch_size;
                for (i, value) in chunk_entries.into_iter().enumerate() {
                    let pos = chunk_start + i as u64;
                    if pos >= start && pos < chunk_end {
                        entries.push((pos, value));
                    }
                }
            }
        }

        // Buffer tail: positions in [max(start, buffer_start), end). Read
        // through the dense tree directly so each read's cost is charged.
        for pos in start.max(buffer_start)..end {
            let buffer_pos = (pos - buffer_start) as u16;
            let value = match self.dense_tree.get(buffer_pos).unwrap_add_cost(&mut cost) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "missing buffer value at position {}",
                        buffer_pos
                    )))
                    .wrap_with_cost(cost);
                }
                Err(e) => {
                    return Err(BulkAppendError::StorageError(format!(
                        "dense tree get at {} failed: {}",
                        buffer_pos, e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            entries.push((pos, value));
        }

        Ok(RangePage {
            entries,
            total_count,
        })
        .wrap_with_cost(cost)
    }

    // ── Chunk operations (MMR) ───────────────────────────────────────

    /// Get a single completed chunk's raw blob by chunk index.
    ///
    /// This reads from the **chunk MMR**, which stores immutable epoch blobs.
    /// Returns `None` if the chunk hasn't been completed yet.
    ///
    /// Uses the MMR overlay to find nodes that were pushed during this session
    /// but not yet committed to storage.
    pub fn get_chunk_value(&self, chunk_index: u64) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if chunk_index >= self.chunk_count() {
            return Ok(None);
        }
        let mmr_pos = leaf_to_pos(chunk_index);
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
        let mmr = MMR::new_with_overlay(self.mmr_size(), &mmr_store, self.mmr_overlay.clone());
        let node = mmr
            .batch
            .element_at_position(mmr_pos)
            .unwrap()
            .map_err(|e| {
                BulkAppendError::MmrError(format!(
                    "failed to read MMR node for chunk {}: {}",
                    chunk_index, e
                ))
            })?;
        match node {
            Some(n) => Ok(n.into_value()),
            None => Err(BulkAppendError::CorruptedData(format!(
                "missing MMR leaf for chunk {}",
                chunk_index
            ))),
        }
    }

    /// Query completed chunks by their indices.
    ///
    /// This queries the **chunk MMR**, which stores immutable epoch blobs.
    /// Each completed epoch is a single MMR leaf containing all values from
    /// that epoch serialized into a blob.
    ///
    /// Returns a [`ChunkQueryResult`] containing the deserialized chunk
    /// entries and an MMR inclusion proof.
    pub fn query_chunks(&self, chunk_indices: &[u64]) -> Result<ChunkQueryResult, BulkAppendError> {
        let completed_chunks = self.chunk_count();
        let mmr_size = self.mmr_size();

        // Validate indices
        for &idx in chunk_indices {
            if idx >= completed_chunks {
                return Err(BulkAppendError::InvalidInput(format!(
                    "chunk index {} out of range (completed_chunks={})",
                    idx, completed_chunks
                )));
            }
        }

        // Read and deserialize each chunk blob
        let mut chunks = Vec::with_capacity(chunk_indices.len());
        for &idx in chunk_indices {
            let blob = self.get_chunk_value(idx)?.ok_or_else(|| {
                BulkAppendError::CorruptedData(format!("missing chunk blob for index {}", idx))
            })?;
            let entries = deserialize_chunk_blob(&blob)?;
            chunks.push((idx, entries));
        }

        // Generate MMR proof
        let (mmr_proof_items, mmr_root) = if chunk_indices.is_empty() || mmr_size == 0 {
            (Vec::new(), [0u8; 32])
        } else {
            let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
            let mmr = MMR::new_with_overlay(mmr_size, &mmr_store, self.mmr_overlay.clone());

            let positions: Vec<u64> = chunk_indices.iter().map(|&idx| leaf_to_pos(idx)).collect();
            let proof = mmr.gen_proof(positions).unwrap().map_err(|e| {
                BulkAppendError::MmrError(format!("chunk MMR gen_proof failed: {}", e))
            })?;

            let proof_items: Vec<[u8; 32]> =
                proof.proof_items().iter().map(|node| node.hash()).collect();

            let root = mmr.get_root().unwrap().map_err(|e| {
                BulkAppendError::MmrError(format!("chunk MMR get_root failed: {}", e))
            })?;

            (proof_items, root.hash())
        };

        Ok(ChunkQueryResult {
            chunks,
            mmr_proof_items,
            mmr_root,
        })
    }
}
