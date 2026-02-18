//! BulkAppendTree: two-level authenticated append-only structure.
//!
//! - A dense Merkle tree buffer (fixed capacity, power of 2) holds incoming
//!   values
//! - When the buffer fills, entries are serialized into an immutable chunk
//!   blob, the dense Merkle root is computed and appended to a chunk-level MMR
//! - Completed chunk blobs are permanently immutable and CDN-cacheable
//!
//! State root = blake3(mmr_root || buffer_hash) — changes on every append.

mod append;
pub mod hash;
pub mod keys;
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
/// Values are appended to a buffer of fixed size `2^chunk_power`. When the
/// buffer fills, entries are serialized into an immutable chunk blob, a dense
/// Merkle root is computed, and that root is appended to a chunk-level MMR.
///
/// The state root is `blake3(mmr_root || buffer_hash)` and changes on every
/// append. The `buffer_hash` is a running chain: `blake3(prev ||
/// blake3(value))`.
pub struct BulkAppendTree {
    pub(crate) total_count: u64,
    pub(crate) chunk_power: u8,
    pub(crate) mmr_size: u64,
    pub(crate) buffer_hash: [u8; 32],
    pub(crate) mmr_node_cache: RefCell<HashMap<u64, MmrNode>>,
}

impl BulkAppendTree {
    /// Create a new empty tree.
    ///
    /// `chunk_power` is the log2 of the chunk size (e.g. 2 means chunks of 4).
    /// Returns an error if `chunk_power` is greater than 31.
    pub fn new(chunk_power: u8) -> Result<Self, BulkAppendError> {
        if chunk_power > 31 {
            return Err(BulkAppendError::InvalidInput(
                "chunk_power must be <= 31".into(),
            ));
        }
        Ok(Self {
            total_count: 0,
            chunk_power,
            mmr_size: 0,
            buffer_hash: [0u8; 32],
            mmr_node_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Restore from persisted state.
    ///
    /// Returns an error if `chunk_power` is greater than 31.
    pub fn from_state(
        total_count: u64,
        chunk_power: u8,
        mmr_size: u64,
        buffer_hash: [u8; 32],
    ) -> Result<Self, BulkAppendError> {
        if chunk_power > 31 {
            return Err(BulkAppendError::InvalidInput(
                "chunk_power must be <= 31".into(),
            ));
        }
        Ok(Self {
            total_count,
            chunk_power,
            mmr_size,
            buffer_hash,
            mmr_node_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Compute the chunk size from `chunk_power`: `2^chunk_power`.
    pub fn chunk_size(&self) -> u32 {
        1u32 << self.chunk_power
    }

    // ── State accessors ─────────────────────────────────────────────────

    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    pub fn chunk_count(&self) -> u64 {
        self.total_count / self.chunk_size() as u64
    }

    pub fn buffer_count(&self) -> u32 {
        (self.total_count % self.chunk_size() as u64) as u32
    }

    pub fn chunk_power(&self) -> u8 {
        self.chunk_power
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
        let mmr_size = u64::from_be_bytes(
            bytes[0..8]
                .try_into()
                .map_err(|_| BulkAppendError::CorruptedData("bad mmr_size bytes".into()))?,
        );
        let mut buffer_hash = [0u8; 32];
        buffer_hash.copy_from_slice(&bytes[8..40]);
        Ok((mmr_size, buffer_hash))
    }

    /// Load metadata from store and construct a BulkAppendTree from element
    /// fields + stored metadata.
    pub fn load_from_store<S: BulkStore>(
        store: &S,
        total_count: u64,
        chunk_power: u8,
    ) -> Result<Self, BulkAppendError> {
        let meta_result = store
            .get(META_KEY)
            .map_err(|e| BulkAppendError::StorageError(format!("get meta failed: {}", e)))?;
        match meta_result {
            Some(bytes) => {
                let (mmr_size, buffer_hash) = Self::deserialize_meta(&bytes)?;
                Self::from_state(total_count, chunk_power, mmr_size, buffer_hash)
            }
            None => {
                if total_count > 0 {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "total_count is {} but metadata is missing",
                        total_count
                    )));
                }
                Self::new(chunk_power)
            }
        }
    }
}
