//! BulkAppendTree: two-level authenticated append-only structure.
//!
//! - A dense fixed-sized Merkle tree buffer holds incoming values
//! - When the buffer fills, entries are serialized into an immutable chunk blob
//!   and appended to a chunk-level MMR
//! - Completed chunk blobs are permanently immutable and CDN-cacheable
//!
//! State root = blake3("bulk_state" || mmr_root || dense_tree_root) — changes
//! on every append.

#[cfg(feature = "storage")]
mod append;
pub mod hash;

#[cfg(feature = "storage")]
mod fetch;
#[cfg(feature = "storage")]
pub use fetch::{BufferQueryResult, ChunkQueryResult};

#[cfg(all(test, feature = "storage"))]
mod storage_accounting_tests;
#[cfg(all(test, feature = "storage"))]
mod tests;

use grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree;
use grovedb_merkle_mountain_range::MmrNode;

#[cfg(feature = "storage")]
use crate::BulkAppendError;

/// Result returned by `BulkAppendTree::append`.
#[cfg(feature = "storage")]
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
    /// The entry's chunk-blob share, which the caller bills as
    /// `storage_cost.added_bytes`. See
    /// [`AppendNoStateRootResult::prepaid_chunk_bytes`].
    pub prepaid_chunk_bytes: u32,
}

/// Result returned by [`BulkAppendTree::append_no_state_root`].
///
/// Same as [`AppendResult`] minus the state root, which the caller computes
/// once at the end of a batch via
/// [`BulkAppendTree::compute_current_state_root`].
#[cfg(feature = "storage")]
#[derive(Debug, Clone, Copy)]
pub struct AppendNoStateRootResult {
    /// The 0-based global position of the appended value.
    pub global_position: u64,
    /// Number of blake3 hash calls performed during this append (excludes the
    /// deferred state-root computation).
    pub hash_count: u32,
    /// Whether compaction (epoch flush) occurred.
    pub compacted: bool,
    /// The entry's share of the chunk blob it will eventually be compacted
    /// into — its own bytes — to be charged as `storage_cost.added_bytes` at
    /// this append, so that the blob written at compaction can be reported as
    /// a replacement of bytes already paid for (issue #822). `0` under the
    /// shipped accounting (GROVE_V1..V3), where the blob is charged in full
    /// at compaction instead.
    ///
    /// Like `hash_count`, this follows the "caller bills" convention of the
    /// `Result`-returning appends ([`append`](BulkAppendTree::append),
    /// [`append_no_state_root`](BulkAppendTree::append_no_state_root)):
    /// add it to the operation's `storage_cost.added_bytes`. The
    /// `CostResult`-returning
    /// [`append_deferred_roots`](BulkAppendTree::append_deferred_roots)
    /// already includes it in the returned cost; there the field is a
    /// mirror for information only — do not bill it twice.
    pub prepaid_chunk_bytes: u32,
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
    /// MMR node overlay: holds nodes pushed during this session that have
    /// not yet been committed to storage. Persists across MMR instance
    /// lifetimes (compaction cycles) so that reads can find recently-pushed
    /// nodes without a storage round-trip.
    pub(crate) mmr_overlay: Vec<(u64, Vec<MmrNode>)>,
    /// Cached MMR root, refreshed only when a compaction mutates the MMR.
    ///
    /// The MMR is only touched on compaction (every `epoch_size` appends), so
    /// its root is unchanged for the ~`epoch_size - 1` appends in between.
    /// Caching it avoids recomputing the root — and cloning the (blob-bearing)
    /// `mmr_overlay` — on every append, which would otherwise make bulk
    /// appends O(N²) as the overlay grows across compaction cycles.
    ///
    /// `None` means "not yet known" (the state set by [`from_state`], which must
    /// stay lazy: the MMR may not be readable until something is appended). It
    /// is computed once on the first append after an open, then kept in sync by
    /// compaction.
    ///
    /// [`from_state`]: BulkAppendTree::from_state
    pub(crate) last_mmr_root: Option<[u8; 32]>,
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
#[cfg(feature = "storage")]
fn capacity_for_height(height: u8) -> Result<u16, BulkAppendError> {
    if !(1..=16).contains(&height) {
        return Err(BulkAppendError::InvalidInput(format!(
            "height must be between 1 and 16, got {}",
            height
        )));
    }
    Ok(((1u32 << height) - 1) as u16)
}
