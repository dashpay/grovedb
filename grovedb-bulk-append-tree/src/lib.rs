//! BulkAppendTree: a two-level authenticated append-only data structure.
//!
//! Values are buffered in a dense fixed-sized Merkle tree. When the buffer
//! fills, entries are serialized into an immutable chunk blob and appended to
//! a chunk-level MMR.
//!
//! State root = `blake3("bulk_state" || mmr_root || dense_tree_root)` — changes
//! on every append. Completed chunk blobs are permanently immutable and
//! CDN-cacheable.

pub mod chunk;
mod cost;
mod error;
pub mod proof;
mod tree;

#[cfg(all(feature = "storage", any(test, feature = "test-utils")))]
pub mod test_utils;

// Re-export main types
pub use chunk::{chunk_blob_entry_bytes, deserialize_chunk_blob, serialize_chunk_blob};
pub use error::BulkAppendError;
pub use grovedb_dense_fixed_sized_merkle_tree::{DenseFixedSizedMerkleTree, DenseTreeProof};
#[cfg(feature = "storage")]
pub use grovedb_merkle_mountain_range::{MmrKeySize, MmrStore};
pub use proof::{position_range_query, BulkAppendTreeProof, BulkAppendTreeProofResult};
pub use tree::{hash::compute_state_root, leaf_count_to_mmr_size, BulkAppendTree, RangePage};
#[cfg(feature = "storage")]
pub use tree::{AppendNoStateRootResult, AppendResult, BufferQueryResult, ChunkQueryResult};
