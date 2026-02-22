//! Dense fixed-sized Merkle tree using Blake3.
//!
//! A complete binary tree of height h with `2^h - 1` positions, where ALL
//! nodes store data values, filled sequentially in level-order (BFS). No
//! intermediate hashes are stored; the root hash is computed on-the-fly
//! using a uniform hash scheme for every node:
//!
//! `hash = blake3(H(value) || H(left) || H(right))`
//!
//! Nodes without children use `[0; 32]` for both child hashes.

mod error;
pub(crate) mod hash;
pub(crate) mod proof;
#[cfg(feature = "storage")]
mod storage_adapter;
mod store;
pub(crate) mod tree;
mod verify;

#[cfg(test)]
mod tests;

pub use error::DenseMerkleError;
pub use proof::DenseTreeProof;
#[cfg(feature = "storage")]
pub use storage_adapter::{position_key, DenseTreeStorageContext};
pub use store::DenseTreeStore;
pub use tree::DenseFixedSizedMerkleTree;
