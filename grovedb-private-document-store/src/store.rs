//! Storage adapter bridging GroveDB's `StorageContext` to the private
//! document store.
//!
//! Provides [`PrivateDocumentStore`], a thin wrapper owning a
//! [`BulkAppendTree`] plus the committed configuration `{entry_size,
//! chunk_power}`. Unlike the commitment tree there is no Sinsemilla
//! frontier and no extra persisted state: everything lives in the bulk
//! tree; the wrapper adds entry-size validation and the config-binding
//! state root.

use grovedb_bulk_append_tree::BulkAppendTree;
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;

use crate::{
    compute_private_document_store_state_root, private_document_store_config_hash,
    PrivateDocumentStoreError,
};

/// Result of appending to a [`PrivateDocumentStore`].
#[derive(Debug, Clone)]
pub struct PrivateDocumentStoreAppendResult {
    /// The new composite state root
    /// (`blake3("pds_state" || config_hash || bulk_state_root)`).
    /// This flows as the Merk child hash via `insert_subtree`.
    pub state_root: [u8; 32],
    /// The underlying BulkAppendTree state root.
    pub bulk_state_root: [u8; 32],
    /// The 0-based global position of the appended entry.
    pub global_position: u64,
    /// Number of blake3 hash calls performed during the bulk append.
    pub hash_count: u32,
    /// Whether compaction (epoch flush) occurred during this append.
    pub compacted: bool,
}

/// Result of [`PrivateDocumentStore::append_many`].
///
/// Distinct from [`PrivateDocumentStoreAppendResult`] because a batch may be
/// empty, and there is then no appended position to report. Reporting a
/// sentinel (position 0 on a fresh store, or the previous last entry on a
/// populated one) would be indistinguishable from a real append.
#[derive(Debug, Clone)]
pub struct PrivateDocumentStoreAppendManyResult {
    /// The new composite state root.
    pub state_root: [u8; 32],
    /// The underlying BulkAppendTree state root.
    pub bulk_state_root: [u8; 32],
    /// Position of the last entry appended BY THIS CALL, or `None` when the
    /// input was empty and nothing was written.
    pub last_global_position: Option<u64>,
    /// How many entries this call appended.
    pub appended: u64,
    /// Number of blake3 hash calls performed.
    pub hash_count: u32,
    /// Whether compaction occurred during this call.
    pub compacted: bool,
}

/// An append-only store of fixed-size opaque entries.
///
/// Thin wrapper over [`BulkAppendTree`]: appends are validated against the
/// committed `entry_size` and the state root binds the configuration, so a
/// proof can never be reinterpreted under a different config. There is no
/// per-entry delete or update — immutability is enforced by the type.
pub struct PrivateDocumentStore<S> {
    entry_size: u32,
    config_hash: [u8; 32],
    pub(crate) bulk_tree: BulkAppendTree<S>,
}

impl<S> std::fmt::Debug for PrivateDocumentStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateDocumentStore")
            .field("entry_size", &self.entry_size)
            .field("chunk_power", &self.bulk_tree.height())
            .field("total_count", &self.bulk_tree.total_count)
            .finish_non_exhaustive()
    }
}

impl<'db, S: StorageContext<'db>> PrivateDocumentStore<S> {
    /// Create a new empty private document store.
    ///
    /// `entry_size` is the committed byte length of every entry (must be
    /// non-zero). `chunk_power` is the log2 of the epoch size for the
    /// underlying [`BulkAppendTree`] (its dense-buffer height, 1–16).
    pub fn new(
        entry_size: u32,
        chunk_power: u8,
        storage: S,
    ) -> CostResult<Self, PrivateDocumentStoreError> {
        Self::from_state(0, entry_size, chunk_power, storage)
    }

    /// Restore a private document store from persisted state.
    ///
    /// Purely in-memory reconstruction: the bulk tree derives its chunk and
    /// buffer counts from `total_count` and `chunk_power`; no storage reads
    /// happen until an entry is accessed or appended.
    pub fn from_state(
        total_count: u64,
        entry_size: u32,
        chunk_power: u8,
        storage: S,
    ) -> CostResult<Self, PrivateDocumentStoreError> {
        let mut cost = OperationCost::default();
        if entry_size == 0 || entry_size > u16::MAX as u32 {
            return Err(PrivateDocumentStoreError::InvalidConfig(
                "entry_size must be in 1..=65535".to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let bulk_tree = match BulkAppendTree::from_state(total_count, chunk_power, storage) {
            Ok(t) => t,
            Err(e) => {
                return Err(PrivateDocumentStoreError::InvalidData(format!(
                    "bulk tree: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        // Opening the store derives the committed-config hash, which is one
        // blake3 call — charged here so every path that opens a store pays
        // for it rather than getting it for free.
        cost.hash_node_calls += 1;
        Ok(Self {
            entry_size,
            config_hash: private_document_store_config_hash(entry_size, chunk_power),
            bulk_tree,
        })
        .wrap_with_cost(cost)
    }

    /// Append an entry to the store.
    ///
    /// Validates that `entry.len()` equals the committed `entry_size` before
    /// any mutation, then appends to the underlying [`BulkAppendTree`] and
    /// returns the new composite state root.
    pub fn append(
        &mut self,
        entry: &[u8],
    ) -> CostResult<PrivateDocumentStoreAppendResult, PrivateDocumentStoreError> {
        let mut cost = OperationCost::default();

        // Size validation is NOT repeated here: `append_many` validates every
        // entry before writing any, and returns the same
        // `InvalidEntrySize { expected, actual }` with the same (empty) cost.
        // A second copy here would be one more place to drift.
        //
        // Run the single entry through the batch path rather than
        // `BulkAppendTree::append`.
        //
        // `BulkAppendTree::append` walks the dense buffer TWICE per entry:
        // once inside `append_no_state_root`, whose dense root nothing here
        // reads, and again in `compute_current_state_root` to derive the root
        // we actually return. It also returns a plain `Result`, so the second
        // walk's storage reads and hash calls are discarded outright — a
        // one-entry append billed 4 hash calls while performing 6. The
        // deferred path walks once and bills what it walks, and sharing one
        // implementation keeps the single and batch paths from drifting.
        let many = match self
            .append_many(core::iter::once(entry))
            .unwrap_add_cost(&mut cost)
        {
            Ok(r) => r,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };

        // Exactly one entry was supplied and `append_many` returned `Ok`, so
        // it appended it and recorded the position. `expect` rather than an
        // error arm: a `None` here would mean `append_many` reported success
        // without appending, which is a broken postcondition in this file, not
        // a runtime condition a caller can produce or handle.
        let global_position = many
            .last_global_position
            .expect("append_many returned Ok for one entry without a position");

        Ok(PrivateDocumentStoreAppendResult {
            state_root: many.state_root,
            bulk_state_root: many.bulk_state_root,
            global_position,
            hash_count: many.hash_count,
            compacted: many.compacted,
        })
        .wrap_with_cost(cost)
    }

    /// Get an entry by its global 0-based position.
    ///
    /// Returns `None` when `global_position >= total_count`. Dispatches to
    /// the current dense-tree buffer or a completed chunk blob as
    /// appropriate.
    pub fn get_value(
        &self,
        global_position: u64,
    ) -> CostResult<Option<Vec<u8>>, PrivateDocumentStoreError> {
        let mut cost = OperationCost::default();

        if global_position >= self.bulk_tree.total_count {
            return Ok(None).wrap_with_cost(cost);
        }

        let epoch_size = self.bulk_tree.epoch_size();
        let chunk_count = self.bulk_tree.chunk_count();
        let buffer_start = chunk_count * epoch_size;

        let value = if global_position >= buffer_start {
            // Entry is in the current buffer.
            let buffer_pos = (global_position - buffer_start) as u16;
            match self
                .bulk_tree
                .get_buffer_value_with_cost(buffer_pos)
                .unwrap_add_cost(&mut cost)
            {
                Ok(v) => v,
                Err(e) => {
                    return Err(PrivateDocumentStoreError::InvalidData(format!("{}", e)))
                        .wrap_with_cost(cost);
                }
            }
        } else {
            // Entry is in a completed chunk.
            let chunk_idx = global_position / epoch_size;
            let pos_in_chunk = (global_position % epoch_size) as usize;
            let blob = match self
                .bulk_tree
                .get_chunk_value_with_cost(chunk_idx)
                .unwrap_add_cost(&mut cost)
            {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return Err(PrivateDocumentStoreError::CorruptedData(format!(
                        "missing chunk blob for index {}",
                        chunk_idx
                    )))
                    .wrap_with_cost(cost);
                }
                Err(e) => {
                    return Err(PrivateDocumentStoreError::InvalidData(format!("{}", e)))
                        .wrap_with_cost(cost);
                }
            };
            let entries = match grovedb_bulk_append_tree::deserialize_chunk_blob(&blob) {
                Ok(e) => e,
                Err(e) => {
                    return Err(PrivateDocumentStoreError::CorruptedData(format!("{}", e)))
                        .wrap_with_cost(cost);
                }
            };
            // The bounds check above already proved this position exists, so
            // a completed chunk that holds fewer than `epoch_size` entries is
            // corruption — NOT absence. Enforce the same invariant
            // `verify_entry_sizes` checks, otherwise a truncated blob would
            // be reported to the caller as "no such document".
            if entries.len() as u64 != epoch_size {
                return Err(PrivateDocumentStoreError::CorruptedData(format!(
                    "chunk {} has {} entries, expected {}",
                    chunk_idx,
                    entries.len(),
                    epoch_size
                )))
                .wrap_with_cost(cost);
            }
            Some(entries[pos_in_chunk].clone())
        };

        // Defensive: a stored entry that violates the committed entry size
        // indicates corruption (the append path rejects such entries).
        if let Some(v) = &value
            && v.len() != self.entry_size as usize
        {
            return Err(PrivateDocumentStoreError::CorruptedData(format!(
                "entry at position {} has size {}, committed entry size is {}",
                global_position,
                v.len(),
                self.entry_size
            )))
            .wrap_with_cost(cost);
        }

        Ok(value).wrap_with_cost(cost)
    }

    /// Append many entries in one pass, computing roots once at the end.
    ///
    /// Byte-for-byte equivalent to calling [`append`](Self::append) once per
    /// entry — same stored values, same chunk blobs, same final state root —
    /// but O(N) in hash calls instead of O(N^2).
    ///
    /// [`append`](Self::append) recomputes the dense-buffer Merkle root on
    /// every insert, so a run of N buffered appends re-walks every filled
    /// position N times: at `chunk_power = 16` filling one epoch costs about
    /// 4.3 billion blake3 calls for 65,535 entries. This method defers the
    /// dense root, the bulk state root, and the composite `pds_state` root
    /// until the whole run is written.
    ///
    /// # Failure semantics
    ///
    /// EVERY entry is size-validated before ANY is written, so a wrong-sized
    /// entry anywhere in the input leaves the store completely untouched —
    /// no partial write, nothing to roll back.
    ///
    /// A failure that can only be detected mid-run — a storage fault during
    /// a dense-tree write or an MMR compaction — is NOT rolled back: entries
    /// already written stay written. This method has no rollback path of its
    /// own, so a caller that needs all-or-nothing under storage faults must
    /// discard the surrounding transaction, which is what the GroveDB batch
    /// path does. Callers holding no transaction should treat a mid-run
    /// storage error as leaving the store in an indeterminate state.
    ///
    /// Returns post-run roots, the last position appended BY THIS CALL (or
    /// `None` for empty input), how many entries landed, the summed
    /// hash count, and whether any compaction occurred. For an empty input
    /// nothing is written and the current state root is returned.
    pub fn append_many<'e, I>(
        &mut self,
        entries: I,
    ) -> CostResult<PrivateDocumentStoreAppendManyResult, PrivateDocumentStoreError>
    where
        I: IntoIterator<Item = &'e [u8]>,
    {
        let mut cost = OperationCost::default();

        // Validate EVERY entry before writing any of them. Validating inline
        // would let a valid entry land and a later wrong-sized one fail,
        // leaving the store mutated behind an error — which breaks atomicity
        // for a direct caller that has no surrounding transaction to discard.
        let entries: Vec<&[u8]> = entries.into_iter().collect();
        for entry in &entries {
            if entry.len() != self.entry_size as usize {
                return Err(PrivateDocumentStoreError::InvalidEntrySize {
                    expected: self.entry_size,
                    actual: entry.len(),
                })
                .wrap_with_cost(cost);
            }
        }

        let mut hash_count: u32 = 0;
        let mut any_compacted = false;
        let starting_total = self.bulk_tree.total_count;
        let mut last_global_position = None;

        for entry in &entries {
            // The append's own cost — the buffer write, and on a compacting
            // append the read-back of every buffered entry plus the MMR push
            // and root — is merged into `cost` here. Copying only
            // `r.hash_count` into the result field would leave that I/O and
            // the MMR's bagging hashes free.
            let r = match self
                .bulk_tree
                .append_deferred_roots(entry)
                .unwrap_add_cost(&mut cost)
            {
                Ok(r) => r,
                Err(e) => {
                    return Err(PrivateDocumentStoreError::InvalidData(format!(
                        "bulk append: {}",
                        e
                    )))
                    .wrap_with_cost(cost);
                }
            };
            hash_count = hash_count.saturating_add(r.hash_count);
            any_compacted |= r.compacted;
            last_global_position = Some(r.global_position);
        }

        // Pay the deferred roots exactly once, through the cost-aware path
        // so the dense-buffer walk's real storage reads and hashes are
        // billed rather than re-derived from a hand-rolled model.
        let root_ctx = self.bulk_tree.compute_current_state_root_with_cost();
        let root_cost = root_ctx.cost;
        let bulk_state_root = match root_ctx.value {
            Ok(r) => r,
            Err(e) => {
                cost += root_cost;
                return Err(PrivateDocumentStoreError::InvalidData(format!(
                    "state root: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        hash_count = hash_count.saturating_add(root_cost.hash_node_calls);
        cost += root_cost;

        let state_root =
            compute_private_document_store_state_root(&self.config_hash, &bulk_state_root);
        // The composite root is computed on EVERY call, including an empty
        // one, so it is charged unconditionally.
        hash_count = hash_count.saturating_add(1);
        cost.hash_node_calls = cost.hash_node_calls.saturating_add(1);

        Ok(PrivateDocumentStoreAppendManyResult {
            state_root,
            bulk_state_root,
            last_global_position,
            appended: self.bulk_tree.total_count - starting_total,
            hash_count,
            compacted: any_compacted,
        })
        .wrap_with_cost(cost)
    }

    /// Verify that all stored entries respect the committed `entry_size`.
    ///
    /// Walks the current buffer and every completed chunk blob; returns an
    /// error naming the first violating position. Used by GroveDB's
    /// `verify_grovedb` integrity walk. O(total_count) reads — intended for
    /// integrity audits, not hot paths.
    pub fn verify_entry_sizes(&self) -> Result<(), PrivateDocumentStoreError> {
        let expected = self.entry_size as usize;

        // Completed chunks: each blob must deserialize to exactly
        // `epoch_size` entries of `entry_size` bytes.
        let epoch_size = self.bulk_tree.epoch_size();
        for chunk_idx in 0..self.bulk_tree.chunk_count() {
            let blob = self
                .bulk_tree
                .get_chunk_value(chunk_idx)
                .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("{}", e)))?
                .ok_or_else(|| {
                    PrivateDocumentStoreError::CorruptedData(format!(
                        "missing chunk blob for index {}",
                        chunk_idx
                    ))
                })?;
            let entries = grovedb_bulk_append_tree::deserialize_chunk_blob(&blob)
                .map_err(|e| PrivateDocumentStoreError::CorruptedData(format!("{}", e)))?;
            if entries.len() as u64 != epoch_size {
                return Err(PrivateDocumentStoreError::CorruptedData(format!(
                    "chunk {} has {} entries, expected {}",
                    chunk_idx,
                    entries.len(),
                    epoch_size
                )));
            }
            for (i, entry) in entries.iter().enumerate() {
                if entry.len() != expected {
                    return Err(PrivateDocumentStoreError::CorruptedData(format!(
                        "entry at position {} has size {}, committed entry size is {}",
                        chunk_idx * epoch_size + i as u64,
                        entry.len(),
                        expected
                    )));
                }
            }
        }

        // Current buffer.
        let buffer_start = self.bulk_tree.chunk_count() * epoch_size;
        for pos in 0..self.bulk_tree.buffer_count() {
            let entry = self
                .bulk_tree
                .get_buffer_value(pos)
                .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("{}", e)))?
                .ok_or_else(|| {
                    PrivateDocumentStoreError::CorruptedData(format!(
                        "missing buffer entry at position {}",
                        pos
                    ))
                })?;
            if entry.len() != expected {
                return Err(PrivateDocumentStoreError::CorruptedData(format!(
                    "entry at position {} has size {}, committed entry size is {}",
                    buffer_start + pos as u64,
                    entry.len(),
                    expected
                )));
            }
        }

        Ok(())
    }

    /// Compute the composite state root
    /// (`blake3("pds_state" || config_hash || bulk_state_root)`) without
    /// modifying the store.
    pub fn compute_current_state_root(&self) -> Result<[u8; 32], PrivateDocumentStoreError> {
        let bulk_root = self
            .bulk_tree
            .compute_current_state_root()
            .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("state root: {}", e)))?;
        Ok(compute_private_document_store_state_root(
            &self.config_hash,
            &bulk_root,
        ))
    }

    /// Cost-propagating variant of
    /// [`compute_current_state_root`](Self::compute_current_state_root):
    /// charges the underlying dense-root walk (reads and hashes) plus the
    /// composite `pds_state` blake3.
    pub fn compute_current_state_root_with_cost(
        &self,
    ) -> CostResult<[u8; 32], PrivateDocumentStoreError> {
        let mut cost = OperationCost::default();
        let bulk_root = match self
            .bulk_tree
            .compute_current_state_root_with_cost()
            .unwrap_add_cost(&mut cost)
        {
            Ok(r) => r,
            Err(e) => {
                return Err(PrivateDocumentStoreError::InvalidData(format!(
                    "state root: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        cost.hash_node_calls = cost.hash_node_calls.saturating_add(1);
        Ok(compute_private_document_store_state_root(
            &self.config_hash,
            &bulk_root,
        ))
        .wrap_with_cost(cost)
    }

    /// Flush the MMR overlay to storage.
    ///
    /// Delegates to [`BulkAppendTree::commit_mmr`]. Call this at the end of
    /// a session to persist MMR nodes buffered during compaction cycles.
    pub fn commit_mmr(&mut self) -> Result<(), PrivateDocumentStoreError> {
        self.bulk_tree
            .commit_mmr()
            .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("MMR commit: {}", e)))
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// The committed entry size in bytes.
    pub fn entry_size(&self) -> u32 {
        self.entry_size
    }

    /// The chunk power (dense-buffer height) of the underlying bulk tree.
    pub fn chunk_power(&self) -> u8 {
        self.bulk_tree.height()
    }

    /// Total number of entries appended so far.
    pub fn total_count(&self) -> u64 {
        self.bulk_tree.total_count
    }

    /// The number of entries per completed chunk (epoch).
    pub fn epoch_size(&self) -> u64 {
        self.bulk_tree.epoch_size()
    }

    /// Number of completed chunks in the MMR.
    pub fn chunk_count(&self) -> u64 {
        self.bulk_tree.chunk_count()
    }
}

#[cfg(test)]
mod append_many_tests {
    use super::*;
    use grovedb_bulk_append_tree::test_utils::MemStorageContext;

    /// `append_many` must be byte-for-byte equivalent to `append` in a loop:
    /// same state root, same positions, same stored bytes. It only differs in
    /// how many hashes it takes to get there.
    #[test]
    fn append_many_matches_per_entry_append() {
        // 10 entries at chunk_power 2 (epoch size 4) spans two compactions
        // plus a partial buffer, so both storage tiers are exercised.
        let entries: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 8]).collect();

        let mut one_by_one = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("a");
        let mut last = None;
        for e in &entries {
            last = Some(one_by_one.append(e).unwrap().expect("append"));
        }
        let per_entry = last.expect("appended");

        let mut batched = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("b");
        let many = batched
            .append_many(entries.iter().map(|e| e.as_slice()))
            .unwrap()
            .expect("append_many");

        assert_eq!(many.state_root, per_entry.state_root);
        assert_eq!(many.bulk_state_root, per_entry.bulk_state_root);
        assert_eq!(many.last_global_position, Some(per_entry.global_position));
        assert_eq!(many.appended, 10);
        assert_eq!(batched.total_count(), one_by_one.total_count());
        assert_eq!(
            batched.compute_current_state_root().expect("root"),
            one_by_one.compute_current_state_root().expect("root")
        );
        for i in 0..10u64 {
            assert_eq!(
                batched.get_value(i).unwrap().expect("get"),
                one_by_one.get_value(i).unwrap().expect("get"),
                "position {}",
                i
            );
        }
        batched.verify_entry_sizes().expect("sizes ok");

        // The whole point: the batched path does asymptotically less hashing.
        assert!(
            many.hash_count < per_entry.hash_count * 10,
            "append_many should not pay the per-entry dense walk"
        );
    }

    #[test]
    fn append_many_validates_entry_size_and_handles_empty() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");

        // Empty input writes nothing, reports NO appended position (rather
        // than a sentinel indistinguishable from a real append), and still
        // charges the two root hashes it actually performs.
        let empty: Vec<Vec<u8>> = Vec::new();
        let ctx = store.append_many(empty.iter().map(|e| e.as_slice()));
        let empty_cost = ctx.cost.clone();
        let r = ctx.value.expect("empty append_many");
        assert_eq!(store.total_count(), 0);
        assert_eq!(r.last_global_position, None);
        assert_eq!(r.appended, 0);
        assert_eq!(
            r.state_root,
            store.compute_current_state_root().expect("root")
        );
        assert_eq!(
            empty_cost.hash_node_calls, 2,
            "an empty batch still computes the bulk and composite roots"
        );

        // A wrong-size entry is rejected.
        let bad = [vec![0u8; 8], vec![0u8; 7]];
        assert!(matches!(
            store.append_many(bad.iter().map(|e| e.as_slice())).unwrap(),
            Err(PrivateDocumentStoreError::InvalidEntrySize {
                expected: 8,
                actual: 7
            })
        ));
    }
}

#[cfg(test)]
mod atomicity_tests {
    use grovedb_bulk_append_tree::test_utils::MemStorageContext;

    use super::*;

    /// A reopened store resolves its MMR root through the lazy (uncached)
    /// path, which is what proof binding and the integrity walk hit. That
    /// read must be billed, not silently free.
    #[test]
    fn reopened_store_charges_the_uncached_mmr_root_read() {
        let storage = MemStorageContext::new();
        let mut store = PrivateDocumentStore::new(8, 2, storage)
            .unwrap()
            .expect("new");
        // Past a compaction so the MMR actually holds a chunk.
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        // `from_state` leaves the MMR root cache empty, so this computation
        // takes the lazy path.
        let reopened = PrivateDocumentStore::from_state(6, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        let ctx = reopened.compute_current_state_root_with_cost();
        let cost = ctx.cost.clone();
        ctx.value.expect("state root");
        // NOTE: `MemStorageContext::get` reports `OperationCost::default()`,
        // so storage seeks and loaded bytes are invisible to this harness —
        // only the hash accounting is observable here. That the underlying
        // storage reads are genuinely billed is covered against real
        // RocksDB storage by
        // `test_private_document_store_reopened_reads_are_billed` in the
        // grovedb crate.
        // Pinned exactly rather than `> 0`: a loose bound is what let a
        // double-charge of the dense walk sit here unnoticed. With
        // `chunk_power = 2` the buffer holds 3 and an epoch is 4, so 6 total
        // entries leave one completed chunk (mmr_size 1, which takes the
        // single-element path and bags no peaks) and 2 live buffer
        // positions. `hash_node` bills a value hash and a node hash per
        // filled position, so the dense walk is 4; the bulk state root and
        // the composite `pds_state` root are one each.
        assert_eq!(
            cost.hash_node_calls, 6,
            "expected 2*2 dense-walk hashes + 1 bulk state root + 1 composite \
             pds_state root, got {:?}",
            cost
        );
    }

    /// The hash accounting for an append, pinned entry by entry.
    ///
    /// Every figure here is derived from what the code actually hashes, so a
    /// change to either the model or the charging breaks this test rather
    /// than silently shifting fees.
    #[test]
    fn append_bills_exactly_the_hashes_it_performs() {
        // `chunk_power = 4` gives a 15-slot buffer, so none of these appends
        // compacts and the MMR stays empty (its root is the zero hash, taken
        // without hashing).
        let mut store = PrivateDocumentStore::new(8, 4, MemStorageContext::new())
            .unwrap()
            .expect("new");

        // First append: the dense walk visits 1 filled position (2 hashes),
        // then the bulk state root (1) and the composite pds_state root (1).
        let ctx = store.append(&[1u8; 8]);
        ctx.value.expect("append");
        assert_eq!(
            ctx.cost.hash_node_calls, 4,
            "2 dense + 1 bulk root + 1 composite, got {:?}",
            ctx.cost
        );

        // Second append: 2 filled positions now, so the walk costs 4.
        let ctx = store.append(&[2u8; 8]);
        ctx.value.expect("append");
        assert_eq!(
            ctx.cost.hash_node_calls, 6,
            "4 dense + 1 bulk root + 1 composite, got {:?}",
            ctx.cost
        );

        // Third: 6 dense + 2 roots.
        let ctx = store.append(&[3u8; 8]);
        ctx.value.expect("append");
        assert_eq!(
            ctx.cost.hash_node_calls, 8,
            "6 dense + 1 bulk root + 1 composite, got {:?}",
            ctx.cost
        );
    }

    /// Compaction is the expensive branch of an append — it reads every
    /// buffered entry back out of storage, hashes the chunk blob, and pushes
    /// it through the MMR — and all of that used to be discarded, so a
    /// compacting append billed no more I/O than a buffered one.
    #[test]
    fn compacting_append_bills_its_reads_and_hashes() {
        // chunk_power 2: the buffer holds 3, so the 4th append compacts.
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        for i in 0..3u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }

        // The 4th append does not fit the buffer, so it compacts.
        let compacting = store.append(&[3u8; 8]);
        compacting.value.expect("compacting append");
        let compacting_cost = compacting.cost;

        assert!(
            compacting_cost.seek_count > 0 && compacting_cost.storage_loaded_bytes > 0,
            "compaction reads every buffered entry; those reads must be billed, got {:?}",
            compacting_cost
        );
        // 3 buffered entries read back, at the committed 8 bytes each.
        assert!(
            compacting_cost.storage_loaded_bytes >= 24,
            "expected at least the 3 x 8 bytes compaction reads back, got {:?}",
            compacting_cost
        );
        // 1 chunk-blob leaf hash + 1 bulk state root + 1 composite root. The
        // MMR push collapses no peaks at size 0 and the root takes the
        // single-element path, so neither adds a hash here.
        assert_eq!(
            compacting_cost.hash_node_calls, 3,
            "1 leaf + 1 bulk root + 1 composite, got {:?}",
            compacting_cost
        );

        // A plain buffered append afterwards reads nothing back.
        let plain = store.append(&[4u8; 8]);
        plain.value.expect("buffered append");
        assert!(
            plain.cost.storage_loaded_bytes < compacting_cost.storage_loaded_bytes,
            "a buffered append must be cheaper in loaded bytes than a \
             compacting one (buffered {:?} vs compacting {:?})",
            plain.cost,
            compacting_cost
        );
    }

    /// Opening a store derives the committed-config hash, which is real work
    /// and must not be free.
    #[test]
    fn opening_a_store_bills_the_config_hash() {
        let ctx = PrivateDocumentStore::new(8, 4, MemStorageContext::new());
        ctx.value.expect("new");
        assert_eq!(
            ctx.cost.hash_node_calls, 1,
            "the committed-config blake3, got {:?}",
            ctx.cost
        );
    }

    /// A wrong-sized entry anywhere in the batch must leave the store
    /// completely untouched — not partially appended behind an error. A
    /// direct caller has no surrounding transaction to discard.
    #[test]
    fn append_many_is_atomic_on_a_bad_entry() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        store.append(&[1u8; 8]).unwrap().expect("seed");

        let count_before = store.total_count();
        let root_before = store.compute_current_state_root().expect("root");
        let value_before = store.get_value(0).unwrap().expect("get");

        // First entry is valid, second is the wrong size.
        let batch = [vec![2u8; 8], vec![3u8; 7]];
        assert!(matches!(
            store
                .append_many(batch.iter().map(|e| e.as_slice()))
                .unwrap(),
            Err(PrivateDocumentStoreError::InvalidEntrySize {
                expected: 8,
                actual: 7
            })
        ));

        assert_eq!(store.total_count(), count_before, "count must be unchanged");
        assert_eq!(
            store.compute_current_state_root().expect("root"),
            root_before,
            "state root must be unchanged"
        );
        assert_eq!(
            store.get_value(0).unwrap().expect("get"),
            value_before,
            "stored values must be unchanged"
        );
        assert_eq!(
            store.get_value(1).unwrap().expect("get"),
            None,
            "the valid entry preceding the bad one must not have landed"
        );
    }
}

#[cfg(test)]
mod error_path_tests {
    use super::*;
    use grovedb_bulk_append_tree::test_utils::MemStorageContext;

    #[test]
    fn test_debug_and_error_display() {
        let store = PrivateDocumentStore::new(64, 4, MemStorageContext::new())
            .unwrap()
            .expect("new");
        let dbg = format!("{:?}", store);
        assert!(dbg.contains("PrivateDocumentStore") && dbg.contains("entry_size: 64"));
        assert_eq!(store.entry_size(), 64);
        assert_eq!(store.chunk_power(), 4);
        assert_eq!(store.epoch_size(), 16);

        assert!(format!(
            "{}",
            PrivateDocumentStoreError::InvalidEntrySize {
                expected: 8,
                actual: 9
            }
        )
        .contains("expected 8 bytes, got 9"));
        assert!(
            format!("{}", PrivateDocumentStoreError::InvalidConfig("x".into())).contains("config")
        );
        assert!(
            format!("{}", PrivateDocumentStoreError::CorruptedData("x".into()))
                .contains("corrupted")
        );
        assert!(format!("{}", PrivateDocumentStoreError::InvalidData("x".into())).contains("data"));
    }

    #[test]
    fn test_reads_error_on_wiped_storage() {
        // Populate past a compaction so both a chunk and the buffer exist,
        // then wipe the backing storage and reopen with the same claimed
        // state: chunk reads and the integrity walk must surface errors
        // rather than fabricate data.
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);
        storage.data.borrow_mut().clear();

        let broken = PrivateDocumentStore::from_state(6, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        // Position 0 lives in the (now missing) completed chunk.
        assert!(broken.get_value(0).unwrap().is_err());
        // The integrity walk fails on the missing chunk too.
        assert!(broken.verify_entry_sizes().is_err());
        // Buffer positions read as missing entries in the walk; direct
        // get_value returns the underlying error or None consistently.
        assert!(
            broken.compute_current_state_root().is_err() || broken.get_value(5).unwrap().is_err()
        );
    }

    /// Build a store, then reopen the same bytes under a DIFFERENT declared
    /// config. This is the attack the config-binding state root exists to stop,
    /// and the read paths must refuse rather than reinterpret the bytes.
    #[test]
    fn test_reopen_under_wrong_config_is_reported_as_corruption() {
        // 10 entries at chunk_power 3 (epoch 8): one completed chunk of 8,
        // two live in the buffer.
        let mut store = PrivateDocumentStore::new(8, 3, MemStorageContext::new())
            .unwrap()
            .expect("new");
        for i in 0..10u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        // Wrong chunk_power: epoch is now 4, so the stored 8-entry chunk no
        // longer matches the declared epoch. A truncated/oversized chunk must
        // read as corruption, NOT as a missing document.
        let wrong_power = PrivateDocumentStore::from_state(10, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        assert!(
            matches!(
                wrong_power.get_value(0).unwrap(),
                Err(PrivateDocumentStoreError::CorruptedData(ref m))
                    if m.contains("expected 4")
            ),
            "got {:?}",
            wrong_power.get_value(0).unwrap()
        );
        assert!(matches!(
            wrong_power.verify_entry_sizes(),
            Err(PrivateDocumentStoreError::CorruptedData(_))
        ));

        // Wrong entry_size: the chunk deserializes and has the right count,
        // but every entry is the wrong width.
        let storage = PrivateDocumentStore::into_storage_for_test(wrong_power);
        let wrong_size = PrivateDocumentStore::from_state(10, 16, 3, storage)
            .unwrap()
            .expect("reopen");
        assert!(
            matches!(
                wrong_size.verify_entry_sizes(),
                Err(PrivateDocumentStoreError::CorruptedData(ref m))
                    if m.contains("committed entry size is 16")
            ),
            "got {:?}",
            wrong_size.verify_entry_sizes()
        );
        // The same violation is caught on the direct read path.
        assert!(matches!(
            wrong_size.get_value(0).unwrap(),
            Err(PrivateDocumentStoreError::CorruptedData(_))
        ));
    }

    /// A store that claims more entries than its storage holds must report
    /// the absence as corruption on every path — a claimed-but-absent chunk
    /// and a claimed-but-absent buffer slot are different branches.
    #[test]
    fn test_claimed_entries_beyond_storage_are_corruption() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        // 6 entries at chunk_power 2 (epoch 4): one chunk, two buffered.
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        // Claim 20 entries: chunks 1..=3 and their buffer slots do not exist.
        let overclaimed = PrivateDocumentStore::from_state(20, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        // Position 8 sits in chunk 2, which was never written.
        assert!(
            overclaimed.get_value(8).unwrap().is_err(),
            "a claimed-but-absent chunk must not read as success"
        );
        assert!(overclaimed.verify_entry_sizes().is_err());

        // A store claiming buffer entries it never stored: 2 written, 3
        // claimed, and no completed chunk involved.
        let mut store = PrivateDocumentStore::new(8, 3, MemStorageContext::new())
            .unwrap()
            .expect("new");
        for i in 0..2u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        let storage = PrivateDocumentStore::into_storage_for_test(store);
        let overclaimed = PrivateDocumentStore::from_state(3, 8, 3, storage)
            .unwrap()
            .expect("reopen");
        // Surfaces as `InvalidData`, not `CorruptedData`: the dense tree
        // detects the shortfall against its own count and errors before
        // returning `None`, so the store's own "missing buffer entry" arm is
        // defensive rather than reachable from here. What matters is that the
        // walk refuses rather than reporting a short store as intact.
        let err = overclaimed
            .verify_entry_sizes()
            .expect_err("claimed buffer entry does not exist");
        assert!(
            format!("{}", err).contains("position 2"),
            "error should name the missing position, got {:?}",
            err
        );
    }

    /// Deriving the state root over storage that cannot satisfy the claimed
    /// state must surface the error, not a plausible-looking root.
    #[test]
    fn test_state_root_over_missing_storage_errors() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);
        storage.data.borrow_mut().clear();

        let broken = PrivateDocumentStore::from_state(6, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        let ctx = broken.compute_current_state_root_with_cost();
        assert!(
            ctx.value.is_err(),
            "a state root over wiped storage must be an error, got {:?}",
            ctx.value
        );
        // And appending onto that broken state fails rather than writing.
        let mut broken = broken;
        assert!(broken.append(&[9u8; 8]).unwrap().is_err());
    }

    /// Every read path must surface a storage fault instead of reporting the
    /// document as absent. "Not found" and "could not be read" are different
    /// answers, and conflating them on an append-only store would let an I/O
    /// fault look like a legitimately empty position.
    #[test]
    fn test_read_faults_surface_rather_than_reading_as_absent() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        // Past a compaction so both a completed chunk and the buffer exist.
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");

        // Reopen before injecting the fault. The dense tree keeps a
        // write-through cache, so on the original handle a live buffer read is
        // served from memory and never reaches storage — a fault there would
        // prove nothing. A reopened store has a cold cache, which is also the
        // state every real read after a restart is in.
        let storage = PrivateDocumentStore::into_storage_for_test(store);
        storage.fail_reads();
        let mut store = PrivateDocumentStore::from_state(6, 8, 2, storage)
            .unwrap()
            .expect("reopen");

        // Position 5 is in the live buffer, position 0 in a completed chunk:
        // these take different branches and both must error.
        for pos in [5u64, 0] {
            let r = store.get_value(pos).unwrap();
            assert!(
                r.is_err(),
                "position {} must report the read fault, got {:?}",
                pos,
                r
            );
        }

        // The integrity walk and the state-root derivation likewise.
        assert!(store.verify_entry_sizes().is_err());
        assert!(store.compute_current_state_root_with_cost().value.is_err());

        // An out-of-range position is answered before any storage is touched,
        // so it still reports absence rather than the fault.
        assert_eq!(store.get_value(99).unwrap().expect("in-range check"), None);

        store.bulk_tree.dense_tree.storage.heal();
        assert!(store.get_value(0).unwrap().expect("healed read").is_some());
    }

    /// A write fault during an append must fail the append, not report a
    /// success whose state root does not match what was stored.
    #[test]
    fn test_write_faults_fail_the_append() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new");
        store.append(&[0u8; 8]).unwrap().expect("append");

        store.bulk_tree.dense_tree.storage.fail_writes();
        let r = store.append(&[1u8; 8]).unwrap();
        assert!(
            r.is_err(),
            "a failed write must fail the append, got {:?}",
            r
        );

        // The batch path too, and its size prevalidation still runs first: a
        // wrong-sized entry is rejected on its own terms, not as a storage
        // fault.
        let bad = [vec![0u8; 7]];
        assert!(matches!(
            store.append_many(bad.iter().map(|e| e.as_slice())).unwrap(),
            Err(PrivateDocumentStoreError::InvalidEntrySize { .. })
        ));
        let good = [vec![2u8; 8], vec![3u8; 8]];
        assert!(store
            .append_many(good.iter().map(|e| e.as_slice()))
            .unwrap()
            .is_err());
    }
}

#[cfg(test)]
impl<S> PrivateDocumentStore<S> {
    /// Test helper: tear down the store and recover its storage context so a
    /// reopen can be simulated against the same in-memory backing.
    pub(crate) fn into_storage_for_test(store: Self) -> S {
        store.bulk_tree.dense_tree.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grovedb_bulk_append_tree::test_utils::MemStorageContext;

    use crate::{empty_private_document_store_state_root, EMPTY_BULK_APPEND_TREE_STATE_ROOT};

    #[test]
    fn test_empty_store_state_root_matches_helper() {
        let store = PrivateDocumentStore::new(64, 4, MemStorageContext::new())
            .unwrap()
            .expect("new store");
        assert_eq!(
            store.compute_current_state_root().expect("state root"),
            empty_private_document_store_state_root(64, 4),
        );
        // And the inner bulk root of an empty store matches the constant.
        assert_eq!(
            store.bulk_tree.compute_current_state_root().expect("bulk"),
            EMPTY_BULK_APPEND_TREE_STATE_ROOT,
        );
    }

    #[test]
    fn test_zero_entry_size_rejected() {
        assert!(matches!(
            PrivateDocumentStore::new(0, 4, MemStorageContext::new()).unwrap(),
            Err(PrivateDocumentStoreError::InvalidConfig(_))
        ));
    }

    #[test]
    fn test_invalid_chunk_power_rejected() {
        assert!(PrivateDocumentStore::new(64, 0, MemStorageContext::new())
            .unwrap()
            .is_err());
        assert!(PrivateDocumentStore::new(64, 17, MemStorageContext::new())
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_append_validates_entry_size() {
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new store");
        assert!(matches!(
            store.append(&[0u8; 7]).unwrap(),
            Err(PrivateDocumentStoreError::InvalidEntrySize {
                expected: 8,
                actual: 7
            })
        ));
        assert!(matches!(
            store.append(&[0u8; 9]).unwrap(),
            Err(PrivateDocumentStoreError::InvalidEntrySize {
                expected: 8,
                actual: 9
            })
        ));
        // A rejected append must not mutate the store.
        assert_eq!(store.total_count(), 0);
        let ok = store.append(&[1u8; 8]).unwrap().expect("valid append");
        assert_eq!(ok.global_position, 0);
        assert_eq!(store.total_count(), 1);
    }

    #[test]
    fn test_append_get_roundtrip_across_compaction() {
        // chunk_power 2 → capacity 3, epoch size 4: 10 appends span two
        // completed chunks plus a partial buffer.
        let mut store = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("new store");
        let mut roots = Vec::new();
        for i in 0..10u8 {
            let entry = [i; 8];
            let r = store.append(&entry).unwrap().expect("append");
            assert_eq!(r.global_position, i as u64);
            roots.push(r.state_root);
        }
        // Every append must move the state root.
        for w in roots.windows(2) {
            assert_ne!(w[0], w[1]);
        }
        assert_eq!(store.total_count(), 10);
        assert_eq!(store.chunk_count(), 2);

        for i in 0..10u8 {
            let v = store.get_value(i as u64).unwrap().expect("get");
            assert_eq!(v, Some(vec![i; 8]), "position {}", i);
        }
        assert_eq!(store.get_value(10).unwrap().expect("get"), None);

        // The append-path state root matches a fresh computation.
        assert_eq!(
            store.compute_current_state_root().expect("state root"),
            *roots.last().unwrap()
        );

        // And the whole store passes the entry-size integrity walk.
        store.verify_entry_sizes().expect("sizes ok");
    }

    #[test]
    fn test_state_root_differs_from_raw_bulk_root() {
        // The composite root must bind the config: it can never equal the
        // raw bulk root, and two stores with identical data but different
        // configs must have different roots.
        let mut a = PrivateDocumentStore::new(8, 2, MemStorageContext::new())
            .unwrap()
            .expect("a");
        let mut b = PrivateDocumentStore::new(8, 3, MemStorageContext::new())
            .unwrap()
            .expect("b");
        let ra = a.append(&[7u8; 8]).unwrap().expect("append a");
        let rb = b.append(&[7u8; 8]).unwrap().expect("append b");
        assert_ne!(ra.state_root, ra.bulk_state_root);
        assert_ne!(ra.state_root, rb.state_root);
    }

    #[test]
    fn test_reopen_from_state() {
        let storage = MemStorageContext::new();
        let mut store = PrivateDocumentStore::new(8, 2, storage)
            .unwrap()
            .expect("new store");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let root_before = store.compute_current_state_root().expect("root");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        let reopened = PrivateDocumentStore::from_state(6, 8, 2, storage)
            .unwrap()
            .expect("reopen");
        assert_eq!(
            reopened.compute_current_state_root().expect("root"),
            root_before
        );
        for i in 0..6u8 {
            assert_eq!(
                reopened.get_value(i as u64).unwrap().expect("get"),
                Some(vec![i; 8])
            );
        }
        reopened.verify_entry_sizes().expect("sizes ok");
    }

    #[test]
    fn test_verify_entry_sizes_detects_wrong_config() {
        // Write entries of size 8, then reopen claiming entry_size 16 — the
        // integrity walk must flag the first entry.
        let storage = MemStorageContext::new();
        let mut store = PrivateDocumentStore::new(8, 2, storage)
            .unwrap()
            .expect("new store");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        let reopened = PrivateDocumentStore::from_state(6, 16, 2, storage)
            .unwrap()
            .expect("reopen");
        assert!(matches!(
            reopened.verify_entry_sizes(),
            Err(PrivateDocumentStoreError::CorruptedData(_))
        ));
        // get_value performs the same defensive check.
        assert!(reopened.get_value(0).unwrap().is_err());
    }
}
