//! Shared test utilities for the commitment-tree crate.

use incrementalmerkletree::{Hashable, Level};
use orchard::tree::MerkleHashOrchard;

/// Create a deterministic test leaf from an index.
///
/// Produces a valid Pallas field element (32 bytes) that is unique per index.
/// Chains Sinsemilla `combine` calls, one per byte of the index, so the full
/// 64-bit entropy is mixed in and no two distinct indices collide.
pub fn test_leaf(index: u64) -> [u8; 32] {
    let empty = MerkleHashOrchard::empty_leaf();
    let mut current = empty;
    for &byte in index.to_le_bytes().iter() {
        current = MerkleHashOrchard::combine(Level::from(byte), &current, &empty);
    }
    current.to_bytes()
}
