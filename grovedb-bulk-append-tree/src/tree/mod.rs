//! BulkAppendTree: two-level authenticated append-only structure.
//!
//! - A dense fixed-sized Merkle tree buffer holds incoming values
//! - When the buffer fills, entries are serialized into an immutable chunk blob
//!   and appended to a chunk-level MMR
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

use grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree;

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
///
/// Storage is embedded in the dense tree (and shared with the MMR via
/// `MmrStore` adapter), following the same pattern as Merk.
pub struct BulkAppendTree<S> {
    /// Total number of values ever appended across all completed chunks and the
    /// current buffer. Used to derive chunk_count (`total_count / epoch_size`)
    /// and buffer_count (`total_count % epoch_size`), which in turn determine
    /// the MMR size and dense tree state.
    pub total_count: u64,
    pub dense_tree: DenseFixedSizedMerkleTree<S>,
}

impl<S> BulkAppendTree<S> {
    /// The capacity of the dense tree buffer: `2^height - 1`.
    pub fn capacity(&self) -> u16 {
        self.dense_tree.capacity()
    }

    /// The number of entries per completed chunk (epoch).
    ///
    /// Each chunk contains all `capacity` entries from a full dense tree
    /// plus the overflow value that triggered compaction: `capacity + 1 =
    /// 2^height`.
    pub fn epoch_size(&self) -> u64 {
        self.capacity() as u64 + 1
    }

    // ── State accessors ─────────────────────────────────────────────────

    /// Number of completed chunks in the MMR.
    pub fn chunk_count(&self) -> u64 {
        self.total_count / self.epoch_size()
    }

    /// Number of values currently in the buffer.
    pub fn buffer_count(&self) -> u16 {
        self.dense_tree.count()
    }

    /// Height of the dense tree.
    pub fn height(&self) -> u8 {
        self.dense_tree.height()
    }

    /// The internal MMR size, derived from `chunk_count`.
    pub fn mmr_size(&self) -> u64 {
        leaf_count_to_mmr_size(self.chunk_count())
    }

    /// Reference to the internal dense tree.
    pub fn dense_tree(&self) -> &DenseFixedSizedMerkleTree<S> {
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
