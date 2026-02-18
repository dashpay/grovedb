//! BulkAppendTree: a two-level authenticated append-only data structure.
//!
//! Values are buffered in a dense Merkle tree of fixed capacity
//! (`2^chunk_power`). When the buffer fills, entries are serialized into an
//! immutable chunk blob, a dense Merkle root is computed, and that root is
//! appended to a chunk-level MMR.
//!
//! State root = `blake3(mmr_root || buffer_hash)` — changes on every append.
//! Completed chunk blobs are permanently immutable and CDN-cacheable.

pub mod chunk;
mod error;
pub mod proof;
mod store;
mod tree;

// Re-export main types
pub use chunk::{deserialize_chunk_blob, serialize_chunk_blob};
pub use error::BulkAppendError;
pub use proof::{BulkAppendTreeProof, BulkAppendTreeProofResult};
pub use store::{BulkStore, CachedBulkStore};
pub use tree::{
    hash::{chain_buffer_hash, compute_state_root},
    keys::{buffer_key, chunk_key, META_KEY},
    AppendResult, BulkAppendTree,
};
