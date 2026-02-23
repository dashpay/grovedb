//! BulkAppendTree: two-level authenticated append-only structure.
//!
//! - A dense fixed-sized Merkle tree buffer holds incoming values
//! - When the buffer fills, entries are serialized into an immutable chunk
//!   blob and appended to a chunk-level MMR
//! - Completed chunk blobs are permanently immutable and CDN-cacheable
//!
//! State root = blake3("bulk_state" || mmr_root || dense_tree_root) — changes
//! on every append.

mod append;
pub mod hash;

mod query;
pub use query::{BufferQueryResult, ChunkQueryResult};

#[cfg(test)]
mod tests;

use grovedb_dense_fixed_sized_merkle_tree::{DenseFixedSizedMerkleTree, DenseTreeStore};

use crate::BulkAppendError;

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

/// Compute MMR size from leaf count: `2 * n - popcount(n)`.
///
/// This is a well-known MMR property: the total number of nodes (leaves +
/// internal) for an MMR with `n` leaves equals `2n - popcount(n)`, where
/// `popcount` is the number of set bits.
pub fn leaf_count_to_mmr_size(leaf_count: u64) -> u64 {
    if leaf_count == 0 {
        return 0;
    }
    2 * leaf_count - leaf_count.count_ones() as u64
}

/// A two-level authenticated append-only data structure.
///
/// Values are appended to a dense fixed-sized Merkle tree buffer. When the
/// buffer fills, entries are serialized into an immutable chunk blob and the
/// blob is appended as a leaf to a chunk-level MMR.
///
/// The state root is `blake3("bulk_state" || mmr_root || dense_tree_root)` and
/// changes on every append.
pub struct BulkAppendTree<D: DenseTreeStore, M> {
    pub(crate) total_count: u64,
    pub(crate) dense_tree: DenseFixedSizedMerkleTree,
    pub dense_store: D,
    pub mmr_store: M,
}

impl<D: DenseTreeStore, M> BulkAppendTree<D, M> {
    /// Create a new empty tree.
    ///
    /// `height` is the dense tree height (1–16). Capacity = `2^height - 1`.
    pub fn new(height: u8, dense_store: D, mmr_store: M) -> Result<Self, BulkAppendError> {
        let dense_tree = DenseFixedSizedMerkleTree::new(height).map_err(|e| {
            BulkAppendError::InvalidInput(format!("invalid height: {}", e))
        })?;
        Ok(Self {
            total_count: 0,
            dense_tree,
            dense_store,
            mmr_store,
        })
    }

    /// Restore from persisted state.
    ///
    /// `mmr_size` is derived from `total_count` and `epoch_size`.
    /// Dense tree count is derived from `total_count % epoch_size`.
    pub fn from_state(
        total_count: u64,
        height: u8,
        dense_store: D,
        mmr_store: M,
    ) -> Result<Self, BulkAppendError> {
        let capacity = capacity_for_height(height)?;
        let epoch_size = capacity as u64 + 1; // capacity + 1 = 2^height
        let dense_count = (total_count % epoch_size) as u16;
        let dense_tree =
            DenseFixedSizedMerkleTree::from_state(height, dense_count).map_err(|e| {
                BulkAppendError::InvalidInput(format!("invalid dense tree state: {}", e))
            })?;
        Ok(Self {
            total_count,
            dense_tree,
            dense_store,
            mmr_store,
        })
    }

    /// The capacity of the dense tree buffer: `2^height - 1`.
    pub fn capacity(&self) -> u16 {
        self.dense_tree.capacity()
    }

    /// The number of entries per completed chunk (epoch).
    ///
    /// Each chunk contains all `capacity` entries from a full dense tree
    /// plus the overflow value that triggered compaction: `capacity + 1 = 2^height`.
    pub fn epoch_size(&self) -> u64 {
        self.capacity() as u64 + 1
    }

    // ── State accessors ─────────────────────────────────────────────────

    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    pub fn chunk_count(&self) -> u64 {
        self.total_count / self.epoch_size()
    }

    pub fn buffer_count(&self) -> u16 {
        self.dense_tree.count()
    }

    pub fn height(&self) -> u8 {
        self.dense_tree.height()
    }

    /// The internal MMR size, derived from `chunk_count`.
    pub fn mmr_size(&self) -> u64 {
        leaf_count_to_mmr_size(self.chunk_count())
    }

    pub fn dense_tree(&self) -> &DenseFixedSizedMerkleTree {
        &self.dense_tree
    }
}

/// Compute capacity from height: `2^height - 1`.
fn capacity_for_height(height: u8) -> Result<u16, BulkAppendError> {
    if !(1..=16).contains(&height) {
        return Err(BulkAppendError::InvalidInput(format!(
            "height must be between 1 and 16, got {}",
            height
        )));
    }
    Ok(((1u32 << height) - 1) as u16)
}
