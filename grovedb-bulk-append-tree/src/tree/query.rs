//! Read operations for BulkAppendTree.

use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
use grovedb_merkle_mountain_range::{leaf_to_pos, MMRStoreReadOps, MmrKeySize, MmrStore, MMR};
use grovedb_query::Query;
use grovedb_storage::StorageContext;

use super::BulkAppendTree;
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

    // ── Chunk operations (MMR) ───────────────────────────────────────

    /// Get a single completed chunk's raw blob by chunk index.
    ///
    /// This reads from the **chunk MMR**, which stores immutable epoch blobs.
    /// Returns `None` if the chunk hasn't been completed yet.
    pub fn get_chunk_value(&self, chunk_index: u64) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if chunk_index >= self.chunk_count() {
            return Ok(None);
        }
        let mmr_pos = leaf_to_pos(chunk_index);
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
        let node = (&mmr_store)
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
            let mmr = MMR::new(mmr_size, &mmr_store);

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
