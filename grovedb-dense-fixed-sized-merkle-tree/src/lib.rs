//! Dense fixed-sized Merkle tree using Blake3.
//!
//! A complete binary tree of height h with `2^h - 1` positions, where ALL
//! nodes (internal + leaf) store data values, filled sequentially in
//! level-order (BFS). No intermediate hashes are stored; the root hash is
//! computed on-the-fly using recursive hashing:
//! - leaf = `blake3(0x00 || value)`
//! - internal = `blake3(0x01 || value || H(left) || H(right))`

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
pub use hash::{compute_dense_merkle_root, compute_dense_merkle_root_from_values};
pub use proof::DenseTreeProof;
#[cfg(feature = "storage")]
pub use storage_adapter::{position_key, AuxDenseTreeStore};
pub use store::DenseTreeStore;
pub use tree::DenseFixedSizedMerkleTree;
