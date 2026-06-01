//! Append and compaction logic for BulkAppendTree.

use grovedb_merkle_mountain_range::{
    hash_count_for_push, mmr_size_to_leaf_count, MmrKeySize, MmrNode, MmrStore, MMR,
};
use grovedb_storage::StorageContext;

use super::{
    capacity_for_height, hash::compute_state_root, AppendManyResult, AppendNoStateRootResult,
    AppendResult, BulkAppendTree,
};
use crate::{chunk::serialize_chunk_blob, BulkAppendError};

impl<'db, S: StorageContext<'db>> BulkAppendTree<S> {
    /// Create a new empty tree.
    ///
    /// `height` is the dense tree height (1–16). Capacity = `2^height - 1`.
    pub fn new(height: u8, storage: S) -> Result<Self, BulkAppendError> {
        let dense_tree =
            grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree::new(height, storage)
                .map_err(|e| BulkAppendError::InvalidInput(format!("invalid height: {}", e)))?;
        Ok(Self {
            total_count: 0,
            dense_tree,
            mmr_overlay: Vec::new(),
            // Empty tree → empty MMR → zero root.
            last_mmr_root: Some([0u8; 32]),
        })
    }

    /// Restore from persisted state.
    ///
    /// `mmr_size` is derived from `total_count` and `epoch_size`.
    /// Dense tree count is derived from `total_count % epoch_size`.
    pub fn from_state(total_count: u64, height: u8, storage: S) -> Result<Self, BulkAppendError> {
        let capacity = capacity_for_height(height)?;
        let epoch_size = capacity as u64 + 1; // capacity + 1 = 2^height
        let dense_count = (total_count % epoch_size) as u16;
        let dense_tree =
            grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree::from_state(
                height,
                dense_count,
                storage,
            )
            .map_err(|e| {
                BulkAppendError::InvalidInput(format!("invalid dense tree state: {}", e))
            })?;
        Ok(Self {
            total_count,
            dense_tree,
            mmr_overlay: Vec::new(),
            // Lazy: the restored MMR may not be readable until an append occurs,
            // so don't compute the root here. The first append fills the cache.
            last_mmr_root: None,
        })
    }

    /// Append a value to the tree.
    ///
    /// Handles dense tree insert, auto-compaction when the buffer fills, and
    /// state root computation. For batched inserts prefer
    /// [`append_many`](Self::append_many) or [`append_no_state_root`](Self::append_no_state_root)
    /// — they skip the per-leaf state-root blake3 call.
    pub fn append(&mut self, value: &[u8]) -> Result<AppendResult, BulkAppendError> {
        let r = self.append_no_state_root(value)?;
        let state_root = self.compute_current_state_root()?;
        Ok(AppendResult {
            state_root,
            global_position: r.global_position,
            // +1 for the blake3 state-root computation we just did.
            hash_count: r.hash_count.saturating_add(1),
            compacted: r.compacted,
        })
    }

    /// Append a value without computing the per-leaf state root.
    ///
    /// Equivalent to [`append`](Self::append) minus the final
    /// `compute_state_root` blake3 hash. Use inside a batch (typically via
    /// [`append_many`](Self::append_many) or
    /// [`CommitmentTree::append_many_raw`]) and recover the state root once at
    /// the end via [`compute_current_state_root`](Self::compute_current_state_root).
    /// Storage mutation is identical to [`append`](Self::append).
    ///
    /// [`CommitmentTree::append_many_raw`]: ../../grovedb_commitment_tree/struct.CommitmentTree.html#method.append_many_raw
    pub fn append_no_state_root(
        &mut self,
        value: &[u8],
    ) -> Result<AppendNoStateRootResult, BulkAppendError> {
        let mut hash_count: u32 = 0;
        let global_position = self.total_count;

        // 1. Try to insert into the dense tree buffer.
        let try_result = self.dense_tree.try_insert(value).unwrap().map_err(|e| {
            BulkAppendError::StorageError(format!("dense tree insert failed: {}", e))
        })?;

        let compacted = match try_result {
            Some((_dense_root, _position)) => {
                // Inserted successfully, no compaction needed. The MMR is
                // untouched, so its root is unchanged — keep the cache as-is.
                // (On the very first append after a lazy open the cache is
                // `None`; we don't seed it here because we don't need it —
                // the caller will recover the state root via
                // `compute_current_state_root` at the end of the batch, which
                // populates the cache then.)
                hash_count += self.dense_tree.count() as u32 * 2;
                false
            }
            None => {
                // Dense tree is full — compact existing entries + new value.
                // Must run before incrementing total_count so that
                // self.mmr_size() reflects the pre-compaction state.
                let (compact_hashes, mmr_root) = self.compact_with_value(value)?;
                hash_count += compact_hashes;
                // MMR mutated by the compaction — refresh the cached root.
                self.last_mmr_root = Some(mmr_root);
                true
            }
        };

        self.total_count += 1;

        Ok(AppendNoStateRootResult {
            global_position,
            hash_count,
            compacted,
        })
    }

    /// Batch-append a sequence of values, computing the state root **once** at
    /// the end instead of once per leaf.
    ///
    /// Byte-for-byte equivalent to calling [`append`](Self::append) per value
    /// (same dense-tree state, same MMR, same final `state_root`), but skips
    /// the per-leaf blake3 state-root computation. Used by
    /// [`CommitmentTree::append_many_raw`] to keep large bulk seeds linear.
    ///
    /// On error the tree is left partially mutated — discard the surrounding
    /// transaction if you need all-or-nothing semantics.
    ///
    /// [`CommitmentTree::append_many_raw`]: ../../grovedb_commitment_tree/struct.CommitmentTree.html#method.append_many_raw
    pub fn append_many<I>(&mut self, values: I) -> Result<AppendManyResult, BulkAppendError>
    where
        I: IntoIterator<Item = Vec<u8>>,
    {
        let mut appended: u64 = 0;
        let mut hash_count: u64 = 0;
        let mut compactions: u64 = 0;
        let mut last_global_position: Option<u64> = None;

        for value in values {
            let r = self.append_no_state_root(&value)?;
            last_global_position = Some(r.global_position);
            hash_count = hash_count.saturating_add(u64::from(r.hash_count));
            if r.compacted {
                compactions += 1;
            }
            appended += 1;
        }

        // One blake3 for the final state root (matching the +1 each per-leaf
        // `append` would have counted).
        let state_root = self.compute_current_state_root()?;
        if appended > 0 {
            hash_count = hash_count.saturating_add(1);
        }

        Ok(AppendManyResult {
            state_root,
            last_global_position,
            appended,
            hash_count,
            compactions,
        })
    }

    /// Compute the current state root without modifying the tree.
    ///
    /// Uses the cached MMR root when available, so this is O(1) on the
    /// post-first-append fast path (no overlay clone). Falls back to a one-shot
    /// `get_mmr_root` only when the cache is empty (e.g. immediately after a
    /// lazy `from_state` with no appends yet).
    pub fn compute_current_state_root(&self) -> Result<[u8; 32], BulkAppendError> {
        let mmr_root = match self.last_mmr_root {
            Some(r) => r,
            None => self.get_mmr_root()?,
        };
        let dense_root = self.dense_tree.root_hash().unwrap().map_err(|e| {
            BulkAppendError::StorageError(format!("dense tree root_hash failed: {}", e))
        })?;
        Ok(compute_state_root(&mmr_root, &dense_root))
    }

    /// Compact all dense tree entries plus a new value into a chunk blob
    /// and append to the chunk MMR. Resets the dense tree.
    /// Returns `(hash_count, mmr_root)`.
    fn compact_with_value(&mut self, new_value: &[u8]) -> Result<(u32, [u8; 32]), BulkAppendError> {
        let mut hash_count: u32 = 0;
        let count = self.dense_tree.count();

        // Read all existing entries from dense tree
        let mut entries: Vec<Vec<u8>> = Vec::with_capacity(count as usize + 1);
        for i in 0..count {
            let value = self
                .dense_tree
                .get(i)
                .unwrap()
                .map_err(|e| {
                    BulkAppendError::StorageError(format!("dense tree get at {} failed: {}", i, e))
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
        let blob = serialize_chunk_blob(&entries)?;
        let leaf = MmrNode::leaf(blob);

        // Append chunk root to MMR
        let mmr_size = self.mmr_size();
        let leaf_count = mmr_size_to_leaf_count(mmr_size);
        hash_count += hash_count_for_push(leaf_count);

        // Create MmrStore on the fly from the dense tree's storage.
        // Use the overlay from previous compactions so cross-compaction
        // reads work without a storage round-trip. After push+get_root,
        // take the overlay back (don't commit — that happens at session end).
        let mmr_root = {
            let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
            let mut mmr =
                MMR::new_with_overlay(mmr_size, &mmr_store, std::mem::take(&mut self.mmr_overlay));

            let push_result = mmr.push(leaf).unwrap();
            if let Err(e) = push_result {
                // Restore overlay before returning error
                self.mmr_overlay = mmr.batch.take_overlay();
                return Err(BulkAppendError::MmrError(format!("MMR push failed: {}", e)));
            }

            let root_result = mmr.get_root().unwrap();
            let root = match root_result {
                Ok(node) => node.hash(),
                Err(e) => {
                    self.mmr_overlay = mmr.batch.take_overlay();
                    return Err(BulkAppendError::MmrError(format!(
                        "MMR get_root failed: {}",
                        e
                    )));
                }
            };

            // Take overlay back instead of committing
            self.mmr_overlay = mmr.batch.take_overlay();

            root
        };

        // Reset dense tree (old values stay in store, overwritten on next cycle)
        self.dense_tree.reset();

        Ok((hash_count, mmr_root))
    }

    /// Get the MMR root hash, or `[0; 32]` if no chunks exist.
    pub(crate) fn get_mmr_root(&self) -> Result<[u8; 32], BulkAppendError> {
        let mmr_size = self.mmr_size();
        if mmr_size == 0 {
            return Ok([0u8; 32]);
        }
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
        let mmr = MMR::new_with_overlay(mmr_size, &mmr_store, self.mmr_overlay.clone());
        let root_node = mmr
            .get_root()
            .unwrap()
            .map_err(|e| BulkAppendError::MmrError(format!("MMR get_root failed: {}", e)))?;
        Ok(root_node.hash())
    }

    /// Flush the MMR overlay to storage.
    ///
    /// Call this at the end of a session to persist all MMR nodes that were
    /// buffered during compaction cycles. This is a no-op if no compactions
    /// occurred.
    ///
    /// Cost tracking is intentionally omitted at this boundary:
    /// BulkAppendTree returns plain `Result`, not `CostResult`. Storage
    /// I/O costs are captured by the caller's `commit_multi_context_batch`.
    pub fn commit_mmr(&mut self) -> Result<(), BulkAppendError> {
        if self.mmr_overlay.is_empty() {
            return Ok(());
        }
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
        let mut mmr = MMR::new_with_overlay(
            self.mmr_size(),
            &mmr_store,
            std::mem::take(&mut self.mmr_overlay),
        );
        if let Err(e) = mmr.commit().unwrap() {
            // Restore overlay before returning error so retries/get_mmr_root
            // still see the staged nodes.
            self.mmr_overlay = mmr.batch.take_overlay();
            return Err(BulkAppendError::MmrError(format!(
                "MMR commit failed: {}",
                e
            )));
        }
        Ok(())
    }
}
