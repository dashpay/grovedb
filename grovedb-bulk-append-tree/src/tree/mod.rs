//! BulkAppendTree: two-level authenticated append-only structure.
//!
//! - A dense Merkle tree buffer (fixed capacity, power of 2) holds incoming
//!   values
//! - When the buffer fills, entries are serialized into an immutable epoch
//!   blob, the dense Merkle root is computed and appended to an epoch-level MMR
//! - Completed epoch blobs are permanently immutable and CDN-cacheable
//!
//! State root = blake3(mmr_root || buffer_hash) — changes on every append.

mod append;
mod hash;
pub(crate) mod keys;
mod mmr_adapter;
mod query;

#[cfg(test)]
mod tests;

use std::{cell::RefCell, collections::HashMap};

use grovedb_mmr::MmrNode;
use keys::META_KEY;

use crate::{BulkAppendError, BulkStore};

/// Result returned by `BulkAppendTree::append`.
#[derive(Debug, Clone)]
pub struct AppendResult {
    /// The new state root after this append.
    pub state_root: [u8; 32],
    /// The 0-based global position of the appended value.
    pub global_position: u64,
    /// Number of blake3 hash calls performed during this append.
    pub hash_count: u32,
    /// Whether compaction (epoch flush) occurred.
    pub compacted: bool,
}

/// A two-level authenticated append-only data structure.
///
/// Values are appended to a buffer of fixed `epoch_size`. When the buffer
/// fills, entries are serialized into an immutable epoch blob, a dense Merkle
/// root is computed, and that root is appended to an epoch-level MMR.
///
/// The state root is `blake3(mmr_root || buffer_hash)` and changes on every
/// append. The `buffer_hash` is a running chain: `blake3(prev ||
/// blake3(value))`.
pub struct BulkAppendTree {
    pub(crate) total_count: u64,
    pub(crate) epoch_size: u32,
    pub(crate) mmr_size: u64,
    pub(crate) buffer_hash: [u8; 32],
    pub(crate) mmr_node_cache: RefCell<HashMap<u64, MmrNode>>,
}

impl BulkAppendTree {
    /// Create a new empty tree.
    ///
    /// # Panics
    /// Panics if `epoch_size` is not a power of 2 or is 0.
    pub fn new(epoch_size: u32) -> Self {
        assert!(
            epoch_size > 0 && epoch_size.is_power_of_two(),
            "epoch_size must be a power of 2"
        );
        Self {
            total_count: 0,
            epoch_size,
            mmr_size: 0,
            buffer_hash: [0u8; 32],
            mmr_node_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Restore from persisted state.
    pub fn from_state(
        total_count: u64,
        epoch_size: u32,
        mmr_size: u64,
        buffer_hash: [u8; 32],
    ) -> Self {
        Self {
            total_count,
            epoch_size,
            mmr_size,
            buffer_hash,
            mmr_node_cache: RefCell::new(HashMap::new()),
        }
    }

    // ── State accessors ─────────────────────────────────────────────────

    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    pub fn epoch_count(&self) -> u64 {
        self.total_count / self.epoch_size as u64
    }

    pub fn buffer_count(&self) -> u32 {
        (self.total_count % self.epoch_size as u64) as u32
    }

    pub fn epoch_size(&self) -> u32 {
        self.epoch_size
    }

    pub fn mmr_size(&self) -> u64 {
        self.mmr_size
    }

    pub fn buffer_hash(&self) -> [u8; 32] {
        self.buffer_hash
    }

    // ── Metadata persistence ────────────────────────────────────────────

    /// Serialize internal metadata (mmr_size + buffer_hash) to 40 bytes.
    pub fn serialize_meta(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&self.mmr_size.to_be_bytes());
        buf[8..40].copy_from_slice(&self.buffer_hash);
        buf
    }

    /// Deserialize metadata. Returns `(mmr_size, buffer_hash)`.
    pub fn deserialize_meta(bytes: &[u8]) -> Result<(u64, [u8; 32]), BulkAppendError> {
        if bytes.len() != 40 {
            return Err(BulkAppendError::CorruptedData(format!(
                "BulkMeta expected 40 bytes, got {}",
                bytes.len()
            )));
        }
        let mmr_size = u64::from_be_bytes(bytes[0..8].try_into().unwrap());
        let mut buffer_hash = [0u8; 32];
        buffer_hash.copy_from_slice(&bytes[8..40]);
        Ok((mmr_size, buffer_hash))
    }

    /// Load metadata from store and construct a BulkAppendTree from element
    /// fields + stored metadata.
    pub fn load_from_store<S: BulkStore>(
        store: &S,
        total_count: u64,
        epoch_size: u32,
    ) -> Result<Self, BulkAppendError> {
        let meta_result = store
            .get(META_KEY)
            .map_err(|e| BulkAppendError::StorageError(format!("get meta failed: {}", e)))?;
        match meta_result {
            Some(bytes) => {
                let (mmr_size, buffer_hash) = Self::deserialize_meta(&bytes)?;
                Ok(Self::from_state(
                    total_count,
                    epoch_size,
                    mmr_size,
                    buffer_hash,
                ))
            }
            None => Ok(Self::new(epoch_size)),
        }
    }
}
