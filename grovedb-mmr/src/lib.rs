//! MMR (Merkle Mountain Range) integration for GroveDB.
//!
//! Provides a Blake3-based MMR using the `ckb-merkle-mountain-range` crate.
//! MMR is the optimal data structure for append-only authenticated data:
//! zero rotations, O(N) total hashes, sequential I/O.
//!
//! # Architecture
//!
//! - Each node is an `MmrNode` containing a Blake3 hash and optional leaf value
//! - Internal nodes carry only hashes; leaf nodes carry full values
//! - `GroveMmr` wraps the ckb MMR with convenient methods
//! - For GroveDB integration, nodes are persisted to main storage keyed by
//!   position

mod dense_merkle;
mod grove_mmr;
mod node;
pub mod util;

// Re-export useful ckb helpers
pub use ckb_merkle_mountain_range::helper::{
    leaf_index_to_mmr_size as leaf_to_mmr_size, leaf_index_to_pos as leaf_to_pos,
};
// Re-export all public types
pub use dense_merkle::{compute_dense_merkle_root, compute_dense_merkle_root_from_values};
pub use grove_mmr::GroveMmr;
pub use node::{MergeBlake3, MmrNode};
use thiserror::Error;
pub use util::{hash_count_for_push, mmr_size_to_leaf_count};

/// Errors from MMR operations.
#[derive(Debug, Error)]
pub enum MmrError {
    #[error("MMR operation failed: {0}")]
    OperationFailed(String),
    #[error("invalid MMR data: {0}")]
    InvalidData(String),
    #[error("position {0} out of range")]
    PositionOutOfRange(u64),
}
