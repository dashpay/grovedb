//! Append and compaction logic for BulkAppendTree.

use grovedb_dense_fixed_sized_merkle_tree::{DenseFixedSizedMerkleTree, DenseTreeStore};
use grovedb_merkle_mountain_range::{
    hash_count_for_push, mmr_size_to_leaf_count, MMRStoreReadOps, MMRStoreWriteOps, MmrNode, MMR,
};

use super::{hash::compute_state_root, AppendResult, BulkAppendTree};
use crate::{chunk::serialize_chunk_blob, BulkAppendError};

impl<D: DenseTreeStore, M> BulkAppendTree<D, M>
where
    for<'a> &'a M: MMRStoreReadOps + MMRStoreWriteOps,
{
    /// Append a value to the tree.
    ///
    /// Handles dense tree insert, auto-compaction when the buffer fills, and
    /// state root computation.
    pub fn append(&mut self, value: &[u8]) -> Result<AppendResult, BulkAppendError> {
        let mut hash_count: u32 = 0;
        let global_position = self.total_count;

        // 1. Try to insert into the dense tree buffer
        let try_result = self
            .dense_tree
            .try_insert(value, &self.dense_store)
            .unwrap()
            .map_err(|e| {
                BulkAppendError::StorageError(format!("dense tree insert failed: {}", e))
            })?;

        let (compacted, mmr_root, final_dense_root) = match try_result {
            Some((dense_root, _position)) => {
                // Inserted successfully, no compaction needed
                hash_count += self.dense_tree.count() as u32 * 2;
                let root = self.get_mmr_root()?;
                (false, root, dense_root)
            }
            None => {
                // Dense tree is full — compact existing entries + new value.
                // Must run before incrementing total_count so that
                // self.mmr_size() reflects the pre-compaction state.
                let (compact_hashes, mmr_root) = self.compact_with_value(value)?;
                hash_count += compact_hashes;
                (true, mmr_root, [0u8; 32]) // empty tree after reset
            }
        };

        self.total_count += 1;

        // 2. Compute state root (+1 hash)
        let state_root = compute_state_root(&mmr_root, &final_dense_root);
        hash_count += 1;

        Ok(AppendResult {
            state_root,
            global_position,
            hash_count,
            compacted,
        })
    }

    /// Compute the current state root without modifying the tree.
    pub fn compute_current_state_root(&self) -> Result<[u8; 32], BulkAppendError> {
        let mmr_root = self.get_mmr_root()?;
        let dense_root = self
            .dense_tree
            .root_hash(&self.dense_store)
            .unwrap()
            .map_err(|e| {
                BulkAppendError::StorageError(format!("dense tree root_hash failed: {}", e))
            })?;
        Ok(compute_state_root(&mmr_root, &dense_root))
    }

    /// Compact all dense tree entries plus a new value into a chunk blob
    /// and append to the chunk MMR. Resets the dense tree.
    /// Returns `(hash_count, mmr_root)`.
    fn compact_with_value(
        &mut self,
        new_value: &[u8],
    ) -> Result<(u32, [u8; 32]), BulkAppendError> {
        let mut hash_count: u32 = 0;
        let count = self.dense_tree.count();

        // Read all existing entries from dense tree
        let mut entries: Vec<Vec<u8>> = Vec::with_capacity(count as usize + 1);
        for i in 0..count {
            let value = self
                .dense_tree
                .get(i, &self.dense_store)
                .unwrap()
                .map_err(|e| {
                    BulkAppendError::StorageError(format!(
                        "dense tree get at {} failed: {}",
                        i, e
                    ))
                })?
                .ok_or_else(|| {
                    BulkAppendError::CorruptedData(format!(
                        "dense tree missing value at position {} (count={})",
                        i, count
                    ))
                })?;
            entries.push(value);
        }

        // Add the new value that didn't fit
        entries.push(new_value.to_vec());

        // Serialize chunk blob as a standard MMR leaf — hash = blake3(0x00 || blob)
        let blob = serialize_chunk_blob(&entries);
        let leaf = MmrNode::leaf(blob);

        // Append chunk root to MMR
        let mmr_size = self.mmr_size();
        let leaf_count = mmr_size_to_leaf_count(mmr_size);
        hash_count += hash_count_for_push(leaf_count);

        let mut mmr = MMR::new(mmr_size, &self.mmr_store);
        mmr.push(leaf)
            .unwrap()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR push failed: {}", e)))?;

        // Get root BEFORE commit — data is still in the MMRBatch overlay
        let root_node = mmr
            .get_root()
            .unwrap()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR get_root failed: {}", e)))?;
        let mmr_root = root_node.hash();

        mmr.commit()
            .unwrap()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR commit failed: {}", e)))?;

        // Reset dense tree (old values stay in store, overwritten on next cycle)
        self.dense_tree =
            DenseFixedSizedMerkleTree::new(self.dense_tree.height()).map_err(|e| {
                BulkAppendError::InvalidInput(format!("reset dense tree failed: {}", e))
            })?;

        Ok((hash_count, mmr_root))
    }

    /// Get the MMR root hash, or `[0; 32]` if no chunks exist.
    pub(crate) fn get_mmr_root(&self) -> Result<[u8; 32], BulkAppendError> {
        let mmr_size = self.mmr_size();
        if mmr_size == 0 {
            return Ok([0u8; 32]);
        }
        let mmr = MMR::new(mmr_size, &self.mmr_store);
        let root_node = mmr
            .get_root()
            .unwrap()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR get_root failed: {}", e)))?;
        Ok(root_node.hash())
    }
}
