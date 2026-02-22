//! Merkle Mountain Range (MMR) — an append-only authenticated data structure.
//!
//! This crate provides an MMR implementation backed by pluggable stores,
//! using Blake3 domain-separated hashing for all merge operations.
//!
//! # Core types
//!
//! - [`MMR`] — the main MMR struct (push, root, proof, commit).
//! - [`MerkleProof`] — MMR inclusion proof (verify, calculate root).
//! - [`MmrTreeProof`] — GroveDB-specific serializable proof wrapper.
//! - [`MmrNode`] — the element type stored in the MMR.
//!
//! # Store traits
//!
//! - [`MMRStoreReadOps`] — read an element by MMR position.
//! - [`MMRStoreWriteOps`] — persist a contiguous run of elements.
//! - [`MemStore`] — in-memory store (requires `mem_store` feature).
//! - [`MmrStore`] — GroveDB `StorageContext` adapter (requires `storage`
//!   feature).

#![warn(missing_docs)]

mod error;
/// MMR helper functions for position arithmetic, storage keys, and cost
/// calculations.
pub(crate) mod helper;
/// In-memory MMR store (requires `mem_store` feature).
#[cfg(any(test, feature = "mem_store"))]
pub mod mem_store;
mod mmr;
mod mmr_store;
mod node;
mod proof;
#[cfg(feature = "storage")]
mod storage_adapter;
#[cfg(test)]
mod tests;

pub use error::{Error, Result};
pub use grovedb_costs::{CostResult, CostsExt, OperationCost};
pub use helper::{
    hash_count_for_push, leaf_index_to_mmr_size, leaf_index_to_mmr_size as leaf_to_mmr_size,
    leaf_index_to_pos, leaf_index_to_pos as leaf_to_pos, mmr_node_key, mmr_size_to_leaf_count,
};
#[cfg(any(test, feature = "mem_store"))]
pub use mem_store::MemStore;
pub use mmr::MMR;
pub use mmr_store::{MMRBatch, MMRStoreReadOps, MMRStoreWriteOps};
pub use node::{MmrNode, blake3_merge, leaf_hash};
pub use proof::{MerkleProof, MmrTreeProof};
#[cfg(feature = "storage")]
pub use storage_adapter::MmrStore;
