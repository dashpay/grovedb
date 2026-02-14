//! Append and compaction logic for BulkAppendTree.

use ckb_merkle_mountain_range::MMR;
use grovedb_mmr::{
    compute_dense_merkle_root_from_values, hash_count_for_push, mmr_size_to_leaf_count,
    MergeBlake3, MmrNode,
};

use super::{
    hash::{chain_buffer_hash, compute_state_root},
    keys::{buffer_key, epoch_key, META_KEY},
    mmr_adapter::MmrAdapter,
    AppendResult, BulkAppendTree,
};
use crate::{epoch::serialize_epoch_blob, BulkAppendError, BulkStore};

impl BulkAppendTree {
    /// Append a value to the tree.
    ///
    /// Handles buffer write, hash chain update, and auto-compaction when the
    /// buffer fills. Persists all changes through the provided `BulkStore`.
    pub fn append<S: BulkStore>(
        &mut self,
        store: &S,
        value: &[u8],
    ) -> Result<AppendResult, BulkAppendError> {
        let mut hash_count: u32 = 0;

        // 1. Write value to buffer
        let buffer_idx = self.buffer_count();
        let bkey = buffer_key(buffer_idx);
        store
            .put(&bkey, value)
            .map_err(|e| BulkAppendError::StorageError(format!("put buffer failed: {}", e)))?;

        // 2. Update buffer hash chain (+2 hashes)
        self.buffer_hash = chain_buffer_hash(&self.buffer_hash, value);
        hash_count += 2;

        let global_position = self.total_count;
        self.total_count += 1;
        let new_buffer_count = self.buffer_count();

        let compacted;

        // 3. Check if compaction needed
        if new_buffer_count == 0 {
            compacted = true;

            // Read all buffer entries from store
            let mut entries: Vec<Vec<u8>> = Vec::with_capacity(self.epoch_size as usize);
            for i in 0..self.epoch_size {
                let bk = buffer_key(i);
                match store.get(&bk) {
                    Ok(Some(bytes)) => entries.push(bytes),
                    Ok(None) => {
                        return Err(BulkAppendError::CorruptedData(format!(
                            "missing buffer entry {}",
                            i
                        )));
                    }
                    Err(e) => {
                        return Err(BulkAppendError::StorageError(format!(
                            "get buffer failed: {}",
                            e
                        )));
                    }
                }
            }

            hash_count += self.compact_entries(store, &entries)?;
        } else {
            compacted = false;
        }

        // 4. Get MMR root
        let mmr_root = self.get_mmr_root(store)?;

        // 5. Compute state root (+1 hash)
        let state_root = compute_state_root(&mmr_root, &self.buffer_hash);
        hash_count += 1;

        // 6. Save metadata
        let meta_bytes = self.serialize_meta();
        store
            .put(META_KEY, &meta_bytes)
            .map_err(|e| BulkAppendError::StorageError(format!("put meta failed: {}", e)))?;

        Ok(AppendResult {
            state_root,
            global_position,
            hash_count,
            compacted,
        })
    }

    /// Append a value using an in-memory buffer to avoid read-after-write
    /// issues.
    ///
    /// This variant is intended for batch preprocessing where the underlying
    /// store defers writes. The caller provides and maintains a `mem_buffer`
    /// that tracks buffer entries in memory.
    pub fn append_with_mem_buffer<S: BulkStore>(
        &mut self,
        store: &S,
        value: &[u8],
        mem_buffer: &mut Vec<Vec<u8>>,
    ) -> Result<AppendResult, BulkAppendError> {
        let mut hash_count: u32 = 0;

        // 1. Write value to store and in-memory buffer
        let buffer_idx = self.buffer_count();
        let bkey = buffer_key(buffer_idx);
        store
            .put(&bkey, value)
            .map_err(|e| BulkAppendError::StorageError(format!("put buffer failed: {}", e)))?;
        mem_buffer.push(value.to_vec());

        // 2. Update buffer hash chain (+2 hashes)
        self.buffer_hash = chain_buffer_hash(&self.buffer_hash, value);
        hash_count += 2;

        let global_position = self.total_count;
        self.total_count += 1;
        let new_buffer_count = self.buffer_count();

        let compacted;

        // 3. Check if compaction needed
        if new_buffer_count == 0 {
            compacted = true;
            hash_count += self.compact_entries(store, mem_buffer)?;
            mem_buffer.clear();
        } else {
            compacted = false;
        }

        // 4. Compute state root
        let mmr_root = self.get_mmr_root(store)?;
        let state_root = compute_state_root(&mmr_root, &self.buffer_hash);
        hash_count += 1;

        Ok(AppendResult {
            state_root,
            global_position,
            hash_count,
            compacted,
        })
    }

    /// Compact buffer entries into an epoch blob and append the dense Merkle
    /// root to the epoch MMR. Clears the buffer in storage and resets
    /// `buffer_hash`. Returns the number of hash calls performed.
    fn compact_entries<S: BulkStore>(
        &mut self,
        store: &S,
        entries: &[Vec<u8>],
    ) -> Result<u32, BulkAppendError> {
        let mut hash_count: u32 = 0;

        // Compute dense Merkle root
        let entry_refs: Vec<&[u8]> = entries.iter().map(|e| e.as_slice()).collect();
        let (epoch_root, dense_hash_count) = compute_dense_merkle_root_from_values(&entry_refs);
        hash_count += dense_hash_count;

        // Serialize into epoch blob
        let epoch_idx = self.epoch_count() - 1;
        let ekey = epoch_key(epoch_idx);
        let blob = serialize_epoch_blob(entries);
        store
            .put(&ekey, &blob)
            .map_err(|e| BulkAppendError::StorageError(format!("put epoch failed: {}", e)))?;

        // Append epoch root to MMR
        let leaf_count = mmr_size_to_leaf_count(self.mmr_size);
        hash_count += hash_count_for_push(leaf_count);

        let adapter = MmrAdapter {
            store,
            cache: &self.mmr_node_cache,
        };
        let leaf = MmrNode::internal(epoch_root);
        {
            let mut mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &adapter);
            mmr.push(leaf)
                .map_err(|e| BulkAppendError::MmrError(format!("MMR push failed: {}", e)))?;
            mmr.commit()
                .map_err(|e| BulkAppendError::MmrError(format!("MMR commit failed: {}", e)))?;
            self.mmr_size = mmr.mmr_size();
        }

        // Delete buffer entries
        for i in 0..self.epoch_size {
            let bk = buffer_key(i);
            store.delete(&bk).map_err(|e| {
                BulkAppendError::StorageError(format!("delete buffer failed: {}", e))
            })?;
        }

        // Reset buffer hash
        self.buffer_hash = [0u8; 32];

        Ok(hash_count)
    }

    /// Save final metadata to the store. Call after a sequence of
    /// `append_with_mem_buffer` calls to persist the metadata.
    pub fn save_meta<S: BulkStore>(&self, store: &S) -> Result<(), BulkAppendError> {
        let meta_bytes = self.serialize_meta();
        store
            .put(META_KEY, &meta_bytes)
            .map_err(|e| BulkAppendError::StorageError(format!("put meta failed: {}", e)))
    }

    /// Compute the current state root without modifying the tree.
    pub fn compute_current_state_root<S: BulkStore>(
        &self,
        store: &S,
    ) -> Result<[u8; 32], BulkAppendError> {
        let mmr_root = self.get_mmr_root(store)?;
        Ok(compute_state_root(&mmr_root, &self.buffer_hash))
    }

    /// Get the MMR root hash, or `[0; 32]` if no epochs exist.
    pub(crate) fn get_mmr_root<S: BulkStore>(
        &self,
        store: &S,
    ) -> Result<[u8; 32], BulkAppendError> {
        if self.mmr_size == 0 {
            return Ok([0u8; 32]);
        }
        let adapter = MmrAdapter {
            store,
            cache: &self.mmr_node_cache,
        };
        let mmr = MMR::<MmrNode, MergeBlake3, _>::new(self.mmr_size, &adapter);
        let root_node = mmr
            .get_root()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR get_root failed: {}", e)))?;
        Ok(root_node.hash)
    }
}
