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

mod client_memory_commitment_tree;
pub use client_memory_commitment_tree::ClientMemoryCommitmentTree;

#[cfg(feature = "sqlite")]
mod sqlite_store;
#[cfg(feature = "sqlite")]
pub use sqlite_store::{SqliteShardStore, SqliteShardStoreError};

#[cfg(feature = "sqlite")]
mod client_persistent_commitment_tree;
#[cfg(feature = "sqlite")]
pub use client_persistent_commitment_tree::ClientPersistentCommitmentTree;

#[cfg(all(test, feature = "sqlite"))]
mod sqlite_client_tests;
#[cfg(all(test, feature = "sqlite"))]
mod sqlite_store_tests;
#[cfg(test)]
mod tests;
