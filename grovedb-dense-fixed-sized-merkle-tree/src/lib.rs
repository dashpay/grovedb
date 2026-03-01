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

#![warn(missing_docs)]

mod error;
pub(crate) mod hash;
pub(crate) mod proof;
pub(crate) mod tree;
mod verify;

#[cfg(test)]
pub(crate) mod test_utils;
#[cfg(test)]
mod tests;

pub use error::DenseMerkleError;
pub use proof::DenseTreeProof;
pub use tree::{position_key, DenseFixedSizedMerkleTree};
