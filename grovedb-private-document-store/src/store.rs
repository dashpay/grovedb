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
    ) -> Result<Self, PrivateDocumentStoreError> {
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
    ) -> Result<Self, PrivateDocumentStoreError> {
        if entry_size == 0 {
            return Err(PrivateDocumentStoreError::InvalidConfig(
                "entry_size must be non-zero".to_string(),
            ));
        }
        let bulk_tree = BulkAppendTree::from_state(total_count, chunk_power, storage)
            .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("bulk tree: {}", e)))?;
        Ok(Self {
            entry_size,
            config_hash: private_document_store_config_hash(entry_size, chunk_power),
            bulk_tree,
        })
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

        if entry.len() != self.entry_size as usize {
            return Err(PrivateDocumentStoreError::InvalidEntrySize {
                expected: self.entry_size,
                actual: entry.len(),
            })
            .wrap_with_cost(cost);
        }

        let bulk_result = match self.bulk_tree.append(entry) {
            Ok(r) => r,
            Err(e) => {
                return Err(PrivateDocumentStoreError::InvalidData(format!(
                    "bulk append: {}",
                    e
                )))
                .wrap_with_cost(cost);
            }
        };
        cost.hash_node_calls += bulk_result.hash_count;

        let state_root =
            compute_private_document_store_state_root(&self.config_hash, &bulk_result.state_root);

        Ok(PrivateDocumentStoreAppendResult {
            state_root,
            bulk_state_root: bulk_result.state_root,
            global_position: bulk_result.global_position,
            hash_count: bulk_result.hash_count,
            compacted: bulk_result.compacted,
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
    ) -> Result<Option<Vec<u8>>, PrivateDocumentStoreError> {
        if global_position >= self.bulk_tree.total_count {
            return Ok(None);
        }

        let epoch_size = self.bulk_tree.epoch_size();
        let chunk_count = self.bulk_tree.chunk_count();
        let buffer_start = chunk_count * epoch_size;

        let value = if global_position >= buffer_start {
            // Entry is in the current buffer.
            let buffer_pos = (global_position - buffer_start) as u16;
            self.bulk_tree
                .get_buffer_value(buffer_pos)
                .map_err(|e| PrivateDocumentStoreError::InvalidData(format!("{}", e)))?
        } else {
            // Entry is in a completed chunk.
            let chunk_idx = global_position / epoch_size;
            let pos_in_chunk = (global_position % epoch_size) as usize;
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
            entries.get(pos_in_chunk).cloned()
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
            )));
        }

        Ok(value)
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
    use crate::{
        empty_private_document_store_state_root, test_utils::MemStorageContext,
        EMPTY_BULK_APPEND_TREE_STATE_ROOT,
    };

    #[test]
    fn test_empty_store_state_root_matches_helper() {
        let store = PrivateDocumentStore::new(64, 4, MemStorageContext::new()).expect("new store");
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
            PrivateDocumentStore::new(0, 4, MemStorageContext::new()),
            Err(PrivateDocumentStoreError::InvalidConfig(_))
        ));
    }

    #[test]
    fn test_invalid_chunk_power_rejected() {
        assert!(PrivateDocumentStore::new(64, 0, MemStorageContext::new()).is_err());
        assert!(PrivateDocumentStore::new(64, 17, MemStorageContext::new()).is_err());
    }

    #[test]
    fn test_append_validates_entry_size() {
        let mut store =
            PrivateDocumentStore::new(8, 2, MemStorageContext::new()).expect("new store");
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
        let mut store =
            PrivateDocumentStore::new(8, 2, MemStorageContext::new()).expect("new store");
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
            let v = store.get_value(i as u64).expect("get");
            assert_eq!(v, Some(vec![i; 8]), "position {}", i);
        }
        assert_eq!(store.get_value(10).expect("get"), None);

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
        let mut a = PrivateDocumentStore::new(8, 2, MemStorageContext::new()).expect("a");
        let mut b = PrivateDocumentStore::new(8, 3, MemStorageContext::new()).expect("b");
        let ra = a.append(&[7u8; 8]).unwrap().expect("append a");
        let rb = b.append(&[7u8; 8]).unwrap().expect("append b");
        assert_ne!(ra.state_root, ra.bulk_state_root);
        assert_ne!(ra.state_root, rb.state_root);
    }

    #[test]
    fn test_reopen_from_state() {
        let storage = MemStorageContext::new();
        let mut store = PrivateDocumentStore::new(8, 2, storage).expect("new store");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let root_before = store.compute_current_state_root().expect("root");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        let reopened = PrivateDocumentStore::from_state(6, 8, 2, storage).expect("reopen");
        assert_eq!(
            reopened.compute_current_state_root().expect("root"),
            root_before
        );
        for i in 0..6u8 {
            assert_eq!(reopened.get_value(i as u64).expect("get"), Some(vec![i; 8]));
        }
        reopened.verify_entry_sizes().expect("sizes ok");
    }

    #[test]
    fn test_verify_entry_sizes_detects_wrong_config() {
        // Write entries of size 8, then reopen claiming entry_size 16 — the
        // integrity walk must flag the first entry.
        let storage = MemStorageContext::new();
        let mut store = PrivateDocumentStore::new(8, 2, storage).expect("new store");
        for i in 0..6u8 {
            store.append(&[i; 8]).unwrap().expect("append");
        }
        store.commit_mmr().expect("commit mmr");
        let storage = PrivateDocumentStore::into_storage_for_test(store);

        let reopened = PrivateDocumentStore::from_state(6, 16, 2, storage).expect("reopen");
        assert!(matches!(
            reopened.verify_entry_sizes(),
            Err(PrivateDocumentStoreError::CorruptedData(_))
        ));
        // get_value performs the same defensive check.
        assert!(reopened.get_value(0).is_err());
    }
}
