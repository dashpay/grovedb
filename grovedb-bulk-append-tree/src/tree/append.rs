//! Append and compaction logic for BulkAppendTree.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_dense_fixed_sized_merkle_tree::{v1_insert_model_cost, SlotWriteAccounting};
use grovedb_merkle_mountain_range::{mmr_size_to_leaf_count, MmrKeySize, MmrNode, MmrStore, MMR};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use super::{
    capacity_for_height, hash::compute_state_root, AppendNoStateRootResult, AppendResult,
    BufferRecordMismatch, BulkAppendTree,
};
use crate::{
    chunk::serialize_chunk_blob,
    cost::{
        append_storage_accounting, compaction_hash_count, AppendStorageAccounting,
        SlotRewriteAccounting, VARIABLE_ENTRY_FRAMING_BYTES,
    },
    BulkAppendError,
};

/// Storage key of the persisted chunk-MMR root (32 bytes), written by
/// [`BulkAppendTree::commit_mmr`] under the fixed-model accounting so a
/// reopened tree resolves its MMR root with one small read instead of
/// bagging the peaks — which are leaf nodes carrying whole chunk blobs, so
/// bagging reads every peak's blob back. A 1-byte key: the 2-byte slots,
/// 3-byte records, 4-byte MMR nodes and the owners' named keys cannot
/// collide with it.
pub const MMR_ROOT_KEY: &[u8] = b"r";

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
            fixed_entry_size: None,
        })
    }

    /// Declare that every entry of this tree is exactly `entry_size` bytes:
    /// an append of any other length is rejected with `InvalidInput` before
    /// anything is written, every chunk blob therefore takes the fixed
    /// format, and the fixed cost model (GROVE_V4) charges no per-entry
    /// blob framing. Owners that enforce one entry size anyway
    /// (`CommitmentTree`, `PrivateDocumentStore`) declare it so their
    /// appends are charged exactly their long-term bytes; a tree without the
    /// declaration is charged the variable format's four-byte prefix per
    /// entry as a bound.
    pub fn with_fixed_entry_size(mut self, entry_size: u32) -> Self {
        self.fixed_entry_size = Some(entry_size);
        self
    }

    /// The per-entry chunk-blob framing this tree's entries are charged:
    /// none under a declared fixed entry size, the variable format's prefix
    /// otherwise.
    fn entry_framing_bytes(&self) -> u32 {
        if self.fixed_entry_size.is_some() {
            0
        } else {
            VARIABLE_ENTRY_FRAMING_BYTES
        }
    }

    /// Reject a value that breaks the declared fixed entry size — before any
    /// write, so a rejected append leaves the tree untouched.
    fn check_entry_size(&self, value: &[u8]) -> Result<(), BulkAppendError> {
        if let Some(expected) = self.fixed_entry_size
            && value.len() != expected as usize
        {
            return Err(BulkAppendError::InvalidInput(format!(
                "entry has {} bytes, the tree's fixed entry size is {}",
                value.len(),
                expected
            )));
        }
        Ok(())
    }

    /// Restore from persisted state.
    ///
    /// `mmr_size` is derived from `total_count` and `epoch_size`.
    /// Dense tree count is derived from `total_count % epoch_size`.
    pub fn from_state(total_count: u64, height: u8, storage: S) -> Result<Self, BulkAppendError> {
        let capacity = capacity_for_height(height)?;
        let epoch_size = capacity as u64 + 1; // capacity + 1 = 2^height
        let dense_count = (total_count % epoch_size) as u16;
        let mut dense_tree =
            grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree::from_state(
                height,
                dense_count,
                storage,
            )
            .map_err(|e| {
                BulkAppendError::InvalidInput(format!("invalid dense tree state: {}", e))
            })?;
        // The buffer's hash records (root-maintenance version 1) are tagged
        // with the epoch they belong to. Epochs reuse the same slot keys, so
        // the tag is the chunk count: a record left by an earlier epoch then
        // carries a smaller tag and is never trusted. `reset` advances it in
        // step with each compaction.
        dense_tree.set_generation(total_count / epoch_size);
        Ok(Self {
            total_count,
            dense_tree,
            mmr_overlay: Vec::new(),
            // Lazy: the restored MMR may not be readable until an append occurs,
            // so don't compute the root here. The first append fills the cache.
            last_mmr_root: None,
            fixed_entry_size: None,
        })
    }

    /// Decide how the next buffer-slot write — and the path record written
    /// beside it — is reported to the storage cost layer, per the
    /// accounting version: as new storage (the shipped accounting) or as
    /// churn (GROVE_V4: an in-place replacement of its own size, nothing
    /// added, nothing read to size it — the buffer is a rolling scratch area
    /// and the entry's long-term bytes are its prepaid chunk-blob share).
    fn slot_write_accounting(&self, accounting: &AppendStorageAccounting) -> SlotWriteAccounting {
        match accounting.slot_rewrite {
            SlotRewriteAccounting::AsNew => SlotWriteAccounting::AsNew,
            SlotRewriteAccounting::Churn => SlotWriteAccounting::Churn,
        }
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
        let state_root = self.compute_current_state_root(grove_version)?;
        Ok(AppendResult {
            state_root,
            global_position: r.global_position,
            // +1 for the blake3 state-root computation we just did.
            hash_count: r.hash_count.saturating_add(1),
            compacted: r.compacted,
            storage_accounting_cost: r.storage_accounting_cost,
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
    /// version; what the version selects is the reported `hash_count` (for an
    /// append that compacts) and the storage accounting of the data writes —
    /// the cost information attached to the slot put, and the
    /// `storage_accounting_cost` the caller bills.
    pub fn append_no_state_root(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> Result<AppendNoStateRootResult, BulkAppendError> {
        let mut hash_count: u32 = 0;
        let global_position = self.total_count;
        self.check_entry_size(value)?;
        let accounting = append_storage_accounting(grove_version)?;
        let mut storage_accounting_cost = OperationCost::default();
        let slot_write = self.slot_write_accounting(&accounting);
        self.ensure_mmr_root_resolved(&accounting, grove_version)?;

        // 1. Try to insert into the dense tree buffer.
        //
        // This path returns a plain `Result` and reports its work through
        // `hash_count` (the figure CommitmentTree bills) and
        // `storage_accounting_cost`. Under the fixed model (GROVE_V4) every
        // append — buffered or compacting — is charged the buffer's
        // root-maintenance model for its height plus one amortized
        // compaction blake3, and its reads go through
        // `storage_accounting_cost`; under the shipped accounting the dense
        // walk's hashes and the compaction's hashes are reported where they
        // happen and the reads are dropped. The slot and record writes are
        // charged at commit like every other put. Callers that bill
        // everything use `append_deferred_roots`.
        let insert_ctx =
            self.dense_tree
                .try_insert_with_accounting(value, slot_write, grove_version);
        let dense_cost = insert_ctx.cost;
        let try_result = insert_ctx.value.map_err(|e| {
            BulkAppendError::StorageError(format!("dense tree insert failed: {}", e))
        })?;
        if accounting.fixed_model {
            let model = v1_insert_model_cost(self.dense_tree.height());
            hash_count += model.hash_node_calls
                + crate::cost::amortized_compaction_hashes(self.dense_tree.height());
            // The model's record reads plus the compaction's commit-time
            // puts amortized over the epoch (the puts themselves are
            // prepaid: no seek at commit).
            storage_accounting_cost.seek_count = storage_accounting_cost
                .seek_count
                .saturating_add(model.seek_count)
                .saturating_add(crate::cost::amortized_compaction_seeks(
                    self.dense_tree.height(),
                ));
            storage_accounting_cost.storage_loaded_bytes = storage_accounting_cost
                .storage_loaded_bytes
                .saturating_add(model.storage_loaded_bytes);
            // The entry's own bytes as its part of the blob rewrite the
            // epoch's compaction will perform (the blob put itself is
            // prepaid).
            storage_accounting_cost.storage_cost.replaced_bytes = storage_accounting_cost
                .storage_cost
                .replaced_bytes
                .saturating_add(u32::try_from(value.len()).unwrap_or(u32::MAX));
            if try_result.is_none() {
                // A compacting append writes no slot and no record; it is
                // charged their churn — bytes and seeks — all the same, so
                // its figure is the fixed model's.
                storage_accounting_cost.storage_cost.replaced_bytes = storage_accounting_cost
                    .storage_cost
                    .replaced_bytes
                    .saturating_add(self.buffer_churn_replaced_bytes(value.len()));
                storage_accounting_cost.seek_count = storage_accounting_cost
                    .seek_count
                    .saturating_add(crate::cost::BUFFER_CHURN_PUTS);
            }
        } else {
            // The hashes the insert reports: two per filled position (the
            // whole buffer re-walked) — the shipped `count * 2`.
            hash_count += dense_cost.hash_node_calls;
        }

        let compacted = match try_result {
            Some((_dense_root, _position)) => {
                // Inserted successfully, no compaction needed. The MMR is
                // untouched, so its root is unchanged — keep the cache as-is.
                // (On the very first append after a lazy open the cache is
                // `None`; we don't seed it here because we don't need it —
                // the caller will recover the state root via
                // `compute_current_state_root` at the end of the batch, which
                // populates the cache then.)
                false
            }
            None => {
                // Dense tree is full — compact existing entries + new value.
                // Must run before incrementing total_count so that
                // self.mmr_size() reflects the pre-compaction state.
                let (compact_hashes, mmr_root) = self.compact_with_value(value, grove_version)?;
                // Nothing under the fixed model (amortized); the shipped
                // figure otherwise.
                hash_count += compact_hashes;
                // MMR mutated by the compaction — refresh the cached root.
                self.last_mmr_root = Some(mmr_root);
                true
            }
        };

        let epoch_size = self.epoch_size();
        self.total_count += 1;

        storage_accounting_cost.storage_cost.added_bytes = storage_accounting_cost
            .storage_cost
            .added_bytes
            .saturating_add(accounting.prepaid_chunk_bytes(value.len(), self.entry_framing_bytes()))
            .saturating_add(accounting.amortized_compaction_added_bytes(epoch_size));

        Ok(AppendNoStateRootResult {
            global_position,
            hash_count,
            compacted,
            storage_accounting_cost,
        })
    }

    /// Append a value deferring **both** the dense-tree root and the
    /// state root.
    ///
    /// Storage effect is identical to [`append`](Self::append). Under
    /// root-maintenance version 0 (GROVE_V1..V3) the per-insert
    /// `compute_root_hash` walk over the dense buffer is skipped:
    /// [`append_no_state_root`](Self::append_no_state_root) pays that walk on
    /// every call (via `try_insert`), which makes a run of N appends O(N^2)
    /// in hash calls — 65,535 entries at `height = 16` costs ~4.3 billion —
    /// whereas this variant is O(N) plus one final root computation. Under
    /// version 1 (GROVE_V4+) both paths maintain the buffer's per-position
    /// hash records and cost O(height) per append; this variant then differs
    /// only in returning a `CostResult` that bills everything the append did
    /// (the record reads included) rather than the hash count alone.
    ///
    /// The caller MUST recover the state root once at the end via
    /// [`compute_current_state_root`](Self::compute_current_state_root).
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
        if let Err(e) = self.check_entry_size(value) {
            return Err(e).wrap_with_cost(cost);
        }
        let accounting = match append_storage_accounting(grove_version) {
            Ok(a) => a,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        let mut storage_accounting_cost = OperationCost::default();
        let slot_write = self.slot_write_accounting(&accounting);
        if let Err(e) = self.ensure_mmr_root_resolved(&accounting, grove_version) {
            return Err(e).wrap_with_cost(cost);
        }

        let insert_ctx =
            self.dense_tree
                .try_insert_no_root_with_accounting(value, slot_write, grove_version);
        let dense_cost = insert_ctx.cost;
        let try_result = match insert_ctx.value {
            Ok(r) => r,
            Err(e) => {
                cost += dense_cost;
                return Err(BulkAppendError::StorageError(format!(
                    "dense tree insert failed: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        if accounting.fixed_model {
            // The fixed model, whatever the position: the buffer's
            // root-maintenance model plus one amortized compaction blake3,
            // the entry's own bytes as its part of the blob rewrite, and —
            // on a compacting append, which writes no slot and no record —
            // their churn all the same.
            let mut model = v1_insert_model_cost(self.dense_tree.height());
            model.hash_node_calls +=
                crate::cost::amortized_compaction_hashes(self.dense_tree.height());
            model.seek_count =
                model
                    .seek_count
                    .saturating_add(crate::cost::amortized_compaction_seeks(
                        self.dense_tree.height(),
                    ));
            model.storage_cost.replaced_bytes = u32::try_from(value.len()).unwrap_or(u32::MAX);
            if try_result.is_none() {
                model.storage_cost.replaced_bytes = model
                    .storage_cost
                    .replaced_bytes
                    .saturating_add(self.buffer_churn_replaced_bytes(value.len()));
                model.seek_count = model
                    .seek_count
                    .saturating_add(crate::cost::BUFFER_CHURN_PUTS);
            }
            cost += model.clone();
            storage_accounting_cost += model;
        } else {
            cost += dense_cost;
        }

        let compacted = match try_result {
            Some(_position) => false,
            None => {
                // Buffer full — compact existing entries plus this value.
                // Must run before incrementing total_count so self.mmr_size()
                // reflects the pre-compaction state. Under the fixed model
                // the compaction's own work (the read-back, the chunk-leaf
                // hash, the MMR merges and bagging) is amortized into every
                // append and billed nothing here; under the shipped
                // accounting it is billed as performed.
                let compaction = self.compact_with_value_with_cost(value, grove_version);
                let (_model_hash_count, mmr_root) = match compaction.value {
                    Ok(r) => r,
                    Err(e) => {
                        cost += compaction.cost;
                        return Err(e).wrap_with_cost(cost);
                    }
                };
                if !accounting.fixed_model {
                    cost += compaction.cost;
                }
                self.last_mmr_root = Some(mmr_root);
                true
            }
        };

        let epoch_size = self.epoch_size();
        self.total_count += 1;

        // The reported counter is what was billed, so the two cannot
        // disagree (the shipped `hash_count_for_push` model omitted the
        // peak-bagging merges `get_root` performs).
        let hash_count = cost.hash_node_calls;

        // The entry's chunk-blob share (and, under the fixed model, its share
        // of the compaction overhead) is billed here, in the returned cost;
        // the mirror field is informational for this path (see its doc).
        let prepaid_chunk_bytes = accounting
            .prepaid_chunk_bytes(value.len(), self.entry_framing_bytes())
            .saturating_add(accounting.amortized_compaction_added_bytes(epoch_size));
        cost.storage_cost.added_bytes = cost
            .storage_cost
            .added_bytes
            .saturating_add(prepaid_chunk_bytes);
        storage_accounting_cost.storage_cost.added_bytes = storage_accounting_cost
            .storage_cost
            .added_bytes
            .saturating_add(prepaid_chunk_bytes);

        Ok(AppendNoStateRootResult {
            global_position,
            hash_count,
            compacted,
            storage_accounting_cost,
        })
        .wrap_with_cost(cost)
    }

    /// The paid bytes of the slot and the path record a buffered append of a
    /// `value_len`-byte value writes as churn — what a compacting append,
    /// which writes neither, is charged all the same under the fixed model.
    fn buffer_churn_replaced_bytes(&self, value_len: usize) -> u32 {
        let paid = |len: u32| len.saturating_add(len.required_space() as u32);
        let value_len = u32::try_from(value_len).unwrap_or(u32::MAX);
        let record_len =
            grovedb_dense_fixed_sized_merkle_tree::path_record_len(self.dense_tree.height()) as u32;
        paid(value_len).saturating_add(paid(record_len))
    }

    /// Compute the current state root without modifying the tree.
    ///
    /// Uses the cached MMR root when available, so this is O(1) on the
    /// post-first-append fast path (no overlay clone). Otherwise resolves the
    /// MMR root through [`get_mmr_root_with_cost`](Self::get_mmr_root_with_cost)
    /// — the persisted root under the fixed-model accounting, the peak
    /// bagging before it.
    ///
    /// `grove_version` selects how the dense buffer's root is derived (the
    /// value is the same under every version): walked from every filled
    /// position under GROVE_V1..V3, read from the last insert's path record
    /// from GROVE_V4.
    pub fn compute_current_state_root(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<[u8; 32], BulkAppendError> {
        let mmr_root = match self.last_mmr_root {
            Some(r) => r,
            None => self.get_mmr_root_with_cost(grove_version).unwrap()?,
        };
        let dense_root = self
            .dense_tree
            .root_hash(grove_version)
            .unwrap()
            .map_err(|e| {
                BulkAppendError::StorageError(format!("dense tree root_hash failed: {}", e))
            })?;
        Ok(compute_state_root(&mmr_root, &dense_root))
    }

    /// The state root with the dense buffer's root derived from the stored
    /// VALUES alone (never from the GROVE_V4 hash records) — the independent
    /// audit derivation for integrity walks and a restore's binding check.
    /// Identical to [`compute_current_state_root`](Self::compute_current_state_root)
    /// on a consistent tree; see `DenseFixedSizedMerkleTree::root_hash_from_values`.
    pub fn compute_current_state_root_from_values(&self) -> Result<[u8; 32], BulkAppendError> {
        // Bag the peaks: the persisted MMR root is derived state an audit
        // must not trust either.
        let mmr_root = self
            .bag_mmr_root_with_cost(GroveVersion::first())
            .unwrap()?;
        let dense_root = self
            .dense_tree
            .root_hash_from_values()
            .unwrap()
            .map_err(|e| {
                BulkAppendError::StorageError(format!("dense tree root_hash failed: {}", e))
            })?;
        Ok(compute_state_root(&mmr_root, &dense_root))
    }

    /// Audit the tree's derived state — the buffer's path records and the
    /// persisted MMR root (GROVE_V4) — against its values: `Some` when the
    /// state root a normal read under `grove_version` returns disagrees with
    /// the state root walked from the values and bagged from the MMR peaks,
    /// `None` when they agree (a buffer filled under GROVE_V1..V3 with no
    /// records, or a tree without a persisted MMR root, reads through the
    /// same walk and agrees).
    pub fn buffer_record_mismatch(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<Option<BufferRecordMismatch>, BulkAppendError> {
        let recorded = self.compute_current_state_root(grove_version)?;
        let walked = self.compute_current_state_root_from_values()?;
        Ok((recorded != walked).then_some(BufferRecordMismatch { recorded, walked }))
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
        let accounting = match append_storage_accounting(grove_version) {
            Ok(a) => a,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        // Under the fixed model the two root reads — the persisted MMR root
        // and the last insert's path record — are charged as the model
        // (one seek each, their sizes) whether or not this particular state
        // needs them (an in-session cache, an empty MMR, an empty buffer
        // right after a compaction); otherwise as performed.
        let mut work = OperationCost::default();
        let mmr_root = match self.last_mmr_root {
            Some(r) => r,
            // Lazy path: a reopened tree has no cached root, so this read is
            // real I/O.
            None => match self
                .get_mmr_root_with_cost(grove_version)
                .unwrap_add_cost(&mut work)
            {
                Ok(r) => r,
                Err(e) => return Err(e).wrap_with_cost(cost),
            },
        };
        let dense_root = match self
            .dense_tree
            .root_hash(grove_version)
            .unwrap_add_cost(&mut work)
        {
            Ok(r) => r,
            Err(e) => {
                return Err(BulkAppendError::StorageError(format!(
                    "dense tree root_hash failed: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        if accounting.fixed_model {
            cost.seek_count += 2;
            cost.storage_loaded_bytes += 32
                + grovedb_dense_fixed_sized_merkle_tree::path_record_len(self.dense_tree.height())
                    as u64;
        } else {
            // `root_hash` already charged its own work — the walk over every
            // filled position (a value hash and a node hash each) — and the
            // lazy MMR root read its bagging; both reached us through `work`.
            cost += work;
        }
        // The final blake3 combining the MMR and dense roots.
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
    #[cfg(test)]
    pub(crate) fn get_mmr_root(&self) -> Result<[u8; 32], BulkAppendError> {
        // Cost discarded here, so the version is unobservable; pinned to the
        // shipped accounting (bagging) to match the released callers.
        self.bag_mmr_root_with_cost(GroveVersion::first()).unwrap()
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
        if self.mmr_size() == 0 {
            return Ok([0u8; 32]).wrap_with_cost(cost);
        }
        // Under the fixed-model accounting the root written at the last
        // `commit_mmr` is read back (one small read) instead of bagging the
        // peaks, whose leaf nodes carry whole chunk blobs. Absent (a tree
        // whose chunks were all committed before) → bag.
        let accounting = match append_storage_accounting(grove_version) {
            Ok(a) => a,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        if accounting.fixed_model {
            match self
                .dense_tree
                .storage
                .get(MMR_ROOT_KEY)
                .unwrap_add_cost(&mut cost)
            {
                Ok(Some(bytes)) if bytes.len() == 32 => {
                    let mut root = [0u8; 32];
                    root.copy_from_slice(&bytes);
                    return Ok(root).wrap_with_cost(cost);
                }
                // A present value of any other length is not a state this
                // tree ever writes: corruption at the key, not a legacy tree.
                Ok(Some(bytes)) => {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "persisted MMR root has {} bytes, expected 32",
                        bytes.len()
                    )))
                    .wrap_with_cost(cost);
                }
                // Absent: a tree whose last compaction predates the fixed
                // model. Bag the peaks; the append path backfills the key.
                Ok(None) => {}
                Err(e) => {
                    return Err(BulkAppendError::StorageError(format!(
                        "MMR root read failed: {}",
                        e
                    )))
                    .wrap_with_cost(cost);
                }
            }
        }
        self.bag_mmr_root_with_cost(grove_version).add_cost(cost)
    }

    /// Persist the chunk-MMR root under [`MMR_ROOT_KEY`] — prepaid like the
    /// MMR nodes (`KeyValueStorageCost::prepaid()`: no bytes, no seek), so
    /// the append that issues it keeps the fixed model's figure.
    fn persist_mmr_root(&self, root: &[u8; 32]) -> Result<(), BulkAppendError> {
        let cost_info =
            Some(grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost::prepaid());
        self.dense_tree
            .storage
            .put(MMR_ROOT_KEY, root, None, cost_info)
            .unwrap()
            .map_err(|e| BulkAppendError::StorageError(format!("MMR root put failed: {}", e)))
    }

    /// Under the fixed-model accounting, make sure a reopened tree's MMR root
    /// is known before an append: read the persisted root (one of the two
    /// root reads the model charges on every state-root derivation), or —
    /// for a tree whose last compaction predates the fixed model and so has
    /// none — bag the peaks once and BACKFILL the key, prepaid, so the
    /// bagging (which loads a leaf peak's whole chunk blob when the chunk
    /// count is odd) happens at most once per tree and the reads that follow
    /// are the modelled ones. The one-time catch-up is not billed, like the
    /// dense buffer's read-only catch-up; it is bounded by the peak count and
    /// over for good once the key exists. Cached for the session either way.
    fn ensure_mmr_root_resolved(
        &mut self,
        accounting: &AppendStorageAccounting,
        grove_version: &GroveVersion,
    ) -> Result<(), BulkAppendError> {
        if !accounting.fixed_model || self.last_mmr_root.is_some() || self.mmr_size() == 0 {
            return Ok(());
        }
        let persisted = self
            .dense_tree
            .storage
            .get(MMR_ROOT_KEY)
            .unwrap()
            .map_err(|e| BulkAppendError::StorageError(format!("MMR root read failed: {}", e)))?;
        let root = match persisted {
            Some(bytes) if bytes.len() == 32 => {
                let mut root = [0u8; 32];
                root.copy_from_slice(&bytes);
                root
            }
            Some(bytes) => {
                return Err(BulkAppendError::CorruptedData(format!(
                    "persisted MMR root has {} bytes, expected 32",
                    bytes.len()
                )))
            }
            None => {
                let root = self.bag_mmr_root_with_cost(grove_version).unwrap()?;
                self.persist_mmr_root(&root)?;
                root
            }
        };
        self.last_mmr_root = Some(root);
        Ok(())
    }

    /// The MMR root bagged from the peaks — the independent derivation, which
    /// never consults the persisted root. `[0; 32]` for an empty MMR.
    pub(crate) fn bag_mmr_root_with_cost(
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
    /// I/O costs are captured by the caller's `commit_multi_context_batch`,
    /// from the cost information attached to each node's put — which is what
    /// `grove_version` selects: under the shipped accounting every node is
    /// new storage; from GROVE_V4 the chunk blob is reported as a
    /// replacement of the entry bytes the appends already charged.
    pub fn commit_mmr(&mut self, grove_version: &GroveVersion) -> Result<(), BulkAppendError> {
        if self.mmr_overlay.is_empty() {
            return Ok(());
        }
        let accounting = append_storage_accounting(grove_version)?;
        let mmr_store = MmrStore::with_key_size(&self.dense_tree.storage, MmrKeySize::U32)
            .with_leaf_value_storage_cost(accounting.chunk_leaf);
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
        // Under the fixed-model accounting, persist the MMR root the
        // compaction derived (32 bytes) so the next open reads it instead of
        // bagging the peaks' blobs. Prepaid like the MMR nodes: zero-byte
        // cost information, so the compacting append's storage figure stays
        // the fixed model's.
        if accounting.fixed_model
            && let Some(root) = self.last_mmr_root
        {
            self.persist_mmr_root(&root)?;
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
            roots.push(t.compute_current_state_root(version).expect("root"));
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
