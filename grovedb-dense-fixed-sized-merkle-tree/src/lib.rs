//! Dense fixed-sized Merkle tree using Blake3.
//!
//! A complete binary tree of height h with `2^h - 1` positions, where ALL
//! nodes store data values, filled sequentially in level-order (BFS). Every
//! node hashes with one uniform scheme:
//!
//! `hash = blake3(H(value) || H(left) || H(right))`
//!
//! Nodes without children use `[0; 32]` for both child hashes.
//!
//! How the root is maintained is selected by the grove version
//! (`dense_tree_versions.root_maintenance`): under version 0 no intermediate
//! hashes are stored and the root is recomputed from every filled position;
//! under version 1 a per-position hash record is kept beside each value and
//! an insert updates only its ancestor path. The root value is identical
//! under both — see `tree::root_maintenance` (storage feature).

#![deny(missing_docs)]

mod error;
pub(crate) mod hash;
pub(crate) mod proof;
pub(crate) mod tree;
mod verify;

#[cfg(all(test, feature = "storage"))]
mod root_maintenance_tests;
#[cfg(all(test, feature = "storage"))]
pub(crate) mod test_utils;
#[cfg(all(test, feature = "storage"))]
mod tests;

pub use error::DenseMerkleError;
pub use proof::DenseTreeProof;
#[cfg(feature = "storage")]
pub use tree::SlotWriteAccounting;
pub use tree::{position_key, record_key, DenseFixedSizedMerkleTree, HashRecord, HASH_RECORD_LEN};
