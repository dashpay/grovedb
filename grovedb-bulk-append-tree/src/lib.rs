//! BulkAppendTree: a two-level authenticated append-only data structure.
//!
//! Values are buffered in a dense Merkle tree of fixed capacity (`epoch_size`,
//! must be a power of 2). When the buffer fills, entries are serialized into an
//! immutable epoch blob, a dense Merkle root is computed, and that root is
//! appended to an epoch-level MMR.
//!
//! State root = `blake3(mmr_root || buffer_hash)` — changes on every append.
//! Completed epoch blobs are permanently immutable and CDN-cacheable.

pub mod epoch;
mod error;
mod store;
mod tree;

// Re-export main types
pub use epoch::{deserialize_epoch_blob, serialize_epoch_blob};
pub use error::BulkAppendError;
pub use store::BulkStore;
pub use tree::{AppendResult, BulkAppendTree};
