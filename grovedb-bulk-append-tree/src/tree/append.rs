//! Append and compaction logic for BulkAppendTree.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merkle_mountain_range::{mmr_size_to_leaf_count, MmrKeySize, MmrNode, MmrStore, MMR};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{
    capacity_for_height, hash::compute_state_root, AppendNoStateRootResult, AppendResult,
    BulkAppendTree,
};
use crate::{chunk::serialize_chunk_blob, cost::compaction_hash_count, BulkAppendError};

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
    pub fn append(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<AppendResult, BulkAppendError> {
        let r = self.append_no_state_root(value, grove_version)?;
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
    ///
    /// Stored bytes, chunks and roots are identical under every grove
    /// version; only the reported `hash_count` differs, and only for an append
    /// that compacts.
    pub fn append_no_state_root(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
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
                let (compact_hashes, mmr_root) = self.compact_with_value(value, grove_version)?;
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

    /// Append a value deferring **both** the dense-tree root and the
    /// state root.
    ///
    /// Storage effect is identical to [`append`](Self::append), but the
    /// per-insert `compute_root_hash` walk over the dense buffer is skipped.
    /// [`append_no_state_root`](Self::append_no_state_root) still pays that
    /// walk on every call (via `try_insert`), which makes a run of N appends
    /// O(N^2) in hash calls — 65,535 entries at `height = 16` costs ~4.3
    /// billion. This variant is O(N) plus one final root computation.
    ///
    /// The caller MUST recover the state root once at the end via
    /// [`compute_current_state_root`](Self::compute_current_state_root);
    /// until then the dense root is stale in-memory only (it is always
    /// recomputed from stored values, never cached).
    ///
    /// Compaction still happens inline when the buffer fills, because the
    /// chunk blob is built from the stored values, not from the root.
    pub fn append_deferred_roots(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<AppendNoStateRootResult, BulkAppendError> {
        let mut cost = OperationCost::default();
        let global_position = self.total_count;

        let try_result = match self
            .dense_tree
            .try_insert_no_root(value)
            .unwrap_add_cost(&mut cost)
        {
            Ok(r) => r,
            Err(e) => {
                return Err(BulkAppendError::StorageError(format!(
                    "dense tree insert failed: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };

        let compacted = match try_result {
            // Inserted into the buffer; no root walk, so no hashes yet.
            Some(_position) => false,
            None => {
                // Buffer full — compact existing entries plus this value.
                // Must run before incrementing total_count so self.mmr_size()
                // reflects the pre-compaction state.
                // The model counter this returns is deliberately unused — see
                // the `hash_count` derivation below.
                let (_model_hash_count, mmr_root) = match self
                    .compact_with_value_with_cost(value, grove_version)
                    .unwrap_add_cost(&mut cost)
                {
                    Ok(r) => r,
                    Err(e) => return Err(e).wrap_with_cost(cost),
                };
                self.last_mmr_root = Some(mmr_root);
                true
            }
        };

        self.total_count += 1;

        // Derive the reported counter from what was actually billed rather
        // than from `hash_count_for_push`. That helper covers the eager leaf
        // hash and the merges `push` performs, but NOT the peak-bagging
        // merges `get_root` performs during a compaction, so the model
        // counter falls below the true figure as soon as the MMR has more
        // than one peak. Everything accumulated in `cost` here is this
        // append's own hashing, so the two cannot disagree.
        //
        // Scoped to this deferred path on purpose: `compact_with_value` and
        // `append_no_state_root` keep returning the model counter, because
        // the live CommitmentTree adds that value straight into its own
        // `hash_node_calls` and changing it would move a released cost.
        let hash_count = cost.hash_node_calls;

        Ok(AppendNoStateRootResult {
            global_position,
            hash_count,
            compacted,
        })
        .wrap_with_cost(cost)
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

    /// Cost-propagating variant of
    /// [`compute_current_state_root`](Self::compute_current_state_root).
    ///
    /// Identical result; the difference is that the dense-tree root walk's
    /// storage reads and hash calls reach the caller instead of being
    /// discarded, and the final state-root blake3 is charged on top of them.
    /// Callers that bill work — anything returning a `CostResult` — should
    /// prefer this.
    pub fn compute_current_state_root_with_cost(
        &self,
        grove_version: &GroveVersion,
    ) -> CostResult<[u8; 32], BulkAppendError> {
        let mut cost = OperationCost::default();
        let mmr_root = match self.last_mmr_root {
            Some(r) => r,
            // Lazy path: a reopened tree has no cached root, so this read is
            // real I/O and must be billed.
            None => match self
                .get_mmr_root_with_cost(grove_version)
                .unwrap_add_cost(&mut cost)
            {
                Ok(r) => r,
                Err(e) => return Err(e).wrap_with_cost(cost),
            },
        };
        let dense_root = match self.dense_tree.root_hash().unwrap_add_cost(&mut cost) {
            Ok(r) => r,
            Err(e) => {
                return Err(BulkAppendError::StorageError(format!(
                    "dense tree root_hash failed: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        // `root_hash` already charged the walk itself: `hash_node` bills a
        // value hash and a node hash for every filled position it visits, and
        // those reached us through `unwrap_add_cost` above. Only the final
        // blake3 combining the MMR and dense roots is still unbilled.
        cost.hash_node_calls = cost.hash_node_calls.saturating_add(1);
        Ok(compute_state_root(&mmr_root, &dense_root)).wrap_with_cost(cost)
    }

    /// Compact all dense tree entries plus a new value into a chunk blob
    /// and append to the chunk MMR. Resets the dense tree.
    /// Returns `(hash_count, mmr_root)`.
    ///
    /// Cost-discarding wrapper over
    /// [`compact_with_value_with_cost`](Self::compact_with_value_with_cost).
    /// Kept so the released `append_no_state_root` path bills exactly what it
    /// always has — its costs are dropped here, not at the call site.
    fn compact_with_value(
        &mut self,
        new_value: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<(u32, [u8; 32]), BulkAppendError> {
        // The accumulated `OperationCost` is discarded here — this path
        // reports its work through the returned `hash_count` instead, which
        // IS version-dependent and which CommitmentTree bills.
        self.compact_with_value_with_cost(new_value, grove_version)
            .unwrap()
    }

    /// Compact the buffer plus `new_value` into a chunk, propagating cost.
    ///
    /// Compaction is the expensive branch of an append: it reads every
    /// buffered entry back out of storage, hashes the serialized blob, and
    /// pushes it through the MMR. All of that was previously discarded, so a
    /// compacting append looked no more expensive than a buffered one.
    fn compact_with_value_with_cost(
        &mut self,
        new_value: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<(u32, [u8; 32]), BulkAppendError> {
        let mut cost = OperationCost::default();
        let mut hash_count: u32 = 0;
        let count = self.dense_tree.count();

        // Read all existing entries from dense tree
        let mut entries: Vec<Vec<u8>> = Vec::with_capacity(count as usize + 1);
        for i in 0..count {
            let read = self.dense_tree.get(i).unwrap_add_cost(&mut cost);
            let value = match read {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "dense tree missing value at position {} (count={})",
                        i, count
                    )))
                    .wrap_with_cost(cost);
                }
                Err(e) => {
                    return Err(BulkAppendError::StorageError(format!(
                        "dense tree get at {} failed: {}",
                        i, e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            entries.push(value);
        }

        // Add the new value that didn't fit
        entries.push(new_value.to_vec());

        // Serialize chunk blob as a standard MMR leaf — hash = blake3(0x00 || blob)
        let blob = match serialize_chunk_blob(&entries) {
            Ok(b) => b,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        // `MmrNode::leaf` hashes the blob eagerly.
        cost.hash_node_calls = cost.hash_node_calls.saturating_add(1);
        let leaf = MmrNode::leaf(blob);

        // Append chunk root to MMR
        let mmr_size = self.mmr_size();
        let leaf_count = mmr_size_to_leaf_count(mmr_size);
        // Assigned once inside the MMR block below, after the push, so the
        // bagging term is computed from the shape `get_root` actually folded.
        let mmr_size_after_push;

        // Create MmrStore on the fly from the dense tree's storage.
        // Use the overlay from previous compactions so cross-compaction
        // reads work without a storage round-trip. After push+get_root,
        // take the overlay back (don't commit — that happens at session end).
        let mmr_root = {
            let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
            let mut mmr =
                MMR::new_with_overlay(mmr_size, &mmr_store, std::mem::take(&mut self.mmr_overlay));

            let push_result = mmr.push(leaf, grove_version).unwrap_add_cost(&mut cost);
            if let Err(e) = push_result {
                // Restore overlay before returning error
                self.mmr_overlay = mmr.batch.take_overlay();
                return Err(BulkAppendError::MmrError(format!("MMR push failed: {}", e)))
                    .wrap_with_cost(cost);
            }

            let root_result = mmr.get_root(grove_version).unwrap_add_cost(&mut cost);
            let root = match root_result {
                Ok(node) => node.hash(),
                Err(e) => {
                    self.mmr_overlay = mmr.batch.take_overlay();
                    return Err(BulkAppendError::MmrError(format!(
                        "MMR get_root failed: {}",
                        e
                    )))
                    .wrap_with_cost(cost);
                }
            };

            // Take overlay back instead of committing
            mmr_size_after_push = mmr.mmr_size;
            self.mmr_overlay = mmr.batch.take_overlay();

            root
        };

        // Reset dense tree (old values stay in store, overwritten on next cycle)
        self.dense_tree.reset();

        // The reported count is version-gated: v0 is the shipped figure (leaf
        // hash + push collapses), v1 adds the peak bagging the `get_root`
        // above performed. `mmr.mmr_size` is read after the push, which is
        // the shape that root had to fold.
        hash_count = match compaction_hash_count(leaf_count, mmr_size_after_push, grove_version) {
            Ok(h) => hash_count.saturating_add(h),
            Err(e) => return Err(e).wrap_with_cost(cost),
        };

        Ok((hash_count, mmr_root)).wrap_with_cost(cost)
    }

    /// Get the MMR root hash, or `[0; 32]` if no chunks exist.
    pub(crate) fn get_mmr_root(&self) -> Result<[u8; 32], BulkAppendError> {
        // Cost discarded here, so the version is unobservable; pinned to the
        // shipped accounting to match the released callers.
        self.get_mmr_root_with_cost(GroveVersion::first()).unwrap()
    }

    /// Cost-propagating variant of [`get_mmr_root`](Self::get_mmr_root).
    ///
    /// Matters on the lazy path: `from_state` leaves `last_mmr_root` as
    /// `None`, so a REOPENED non-empty tree resolves its root through here —
    /// exactly the case proof binding and the integrity walk hit. Discarding
    /// the read cost there undercharges their storage I/O.
    pub(crate) fn get_mmr_root_with_cost(
        &self,
        grove_version: &GroveVersion,
    ) -> CostResult<[u8; 32], BulkAppendError> {
        let mut cost = OperationCost::default();
        let mmr_size = self.mmr_size();
        if mmr_size == 0 {
            return Ok([0u8; 32]).wrap_with_cost(cost);
        }
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32);
        let mmr = MMR::new_with_overlay(mmr_size, &mmr_store, self.mmr_overlay.clone());
        match mmr.get_root(grove_version).unwrap_add_cost(&mut cost) {
            Ok(root_node) => Ok(root_node.hash()).wrap_with_cost(cost),
            Err(e) => Err(BulkAppendError::MmrError(format!(
                "MMR get_root failed: {}",
                e
            )))
            .wrap_with_cost(cost),
        }
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

#[cfg(test)]
mod compaction_hash_count_gate_tests {
    use grovedb_version::version::{v1::GROVE_V1, v3::GROVE_V3, v4::GROVE_V4};

    use super::*;
    use crate::test_utils::MemStorageContext;

    /// The reported hash count for a compacting append is version-gated: v0
    /// (V1..V3) omits the peak bagging the compaction's own `get_root`
    /// performs, v1 (V4) includes it. Everything else about the append —
    /// stored bytes, chunk contents, roots — must be identical.
    #[test]
    fn compaction_hash_count_gains_the_bagging_term_at_v4() {
        // chunk_power 2 -> epoch 4. Enough appends to compact repeatedly so
        // the MMR passes through single- and multi-peak shapes.
        let build = |version: &_| {
            let mut t = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
            let mut counts = Vec::new();
            let mut roots = Vec::new();
            for i in 0..20u8 {
                let r = t.append_no_state_root(&[i; 8], version).expect("append");
                if r.compacted {
                    counts.push(r.hash_count);
                }
            }
            roots.push(t.compute_current_state_root().expect("root"));
            (counts, roots)
        };

        let (v0_counts, v0_roots) = build(&GROVE_V3);
        let (v1_counts, v1_roots) = build(&GROVE_V4);

        assert_eq!(
            v0_roots, v1_roots,
            "the tree itself must not depend on the cost version"
        );
        assert_eq!(
            v0_counts.len(),
            v1_counts.len(),
            "same number of compactions"
        );

        // v1 is never cheaper, and is strictly dearer on at least one
        // compaction — the ones that landed on a multi-peak MMR.
        let mut saw_increase = false;
        for (i, (a, b)) in v0_counts.iter().zip(v1_counts.iter()).enumerate() {
            assert!(
                b >= a,
                "v1 must never report fewer hashes (compaction {}): v0={} v1={}",
                i,
                a,
                b
            );
            if b > a {
                saw_increase = true;
            }
        }
        assert!(
            saw_increase,
            "expected at least one multi-peak compaction to gain the bagging \
             term: v0={:?} v1={:?}",
            v0_counts, v1_counts
        );

        // V1 and V3 are both v0, so they must agree exactly.
        let (v1_ver_counts, _) = build(&GROVE_V1);
        assert_eq!(
            v0_counts, v1_ver_counts,
            "GROVE_V1 and GROVE_V3 both select the shipped figure"
        );
    }

    /// An unknown charge version must be rejected, not silently treated as one
    /// of the implemented ones.
    #[test]
    fn compaction_hash_count_rejects_unknown_version() {
        let mut bad = GROVE_V4.clone();
        bad.bulk_append_tree_versions.cost.compaction_hash_count = 99;

        let mut t = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
        // Fill the buffer so the next append compacts and reaches the gate.
        for i in 0..3u8 {
            t.append_no_state_root(&[i; 8], &bad)
                .expect("buffered appends do not reach the gate");
        }
        assert!(
            matches!(
                t.append_no_state_root(&[9u8; 8], &bad),
                Err(BulkAppendError::VersionError(_))
            ),
            "a compacting append must reject an unknown charge version"
        );
    }
}
