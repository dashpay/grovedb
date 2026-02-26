//! Client-side commitment tree with full witness generation.
//!
//! This module provides [`ClientMemoryCommitmentTree`], a wrapper around
//! `shardtree::ShardTree` with an in-memory store, pinned to Orchard types.
//! It is intended for wallets and test harnesses that need to generate
//! Merkle path witnesses for spending notes.
//!
//! Enable the `client` feature to use this module:
//! ```toml
//! grovedb-commitment-tree = { version = "4", features = ["client"] }
//! ```

use client_memory_commitment_tree::ClientMemoryCommitmentTree;
mod tests;
mod sqlite_store_tests;
mod sqlite_client_tests;
mod client_persistent_commitment_tree;
mod sqlite_store;
mod client_memory_commitment_tree;

