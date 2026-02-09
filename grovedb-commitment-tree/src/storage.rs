//! Storage backends for the commitment tree.
//!
//! Provides the `ShardStore` trait implementations needed by `ShardTree`.
//! The `MemoryCommitmentStore` is a simple in-memory implementation for
//! testing. Production use will require a RocksDB-backed implementation
//! that maps to GroveDB's prefixed storage.

use orchard::tree::MerkleHashOrchard;
pub use shardtree::store::memory::MemoryShardStore;

/// In-memory commitment tree store for testing.
///
/// Wraps `shardtree::store::memory::MemoryShardStore` with the correct
/// type parameters for Orchard commitment trees.
///
/// The checkpoint ID type is `u32` for simplicity in tests.
pub type MemoryCommitmentStore = MemoryShardStore<MerkleHashOrchard, u32>;

/// Create a new empty in-memory commitment store.
pub fn new_memory_store() -> MemoryCommitmentStore {
    MemoryShardStore::empty()
}
