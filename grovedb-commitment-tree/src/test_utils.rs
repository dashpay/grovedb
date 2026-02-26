//! Shared test utilities for the commitment-tree crate.

use incrementalmerkletree::{Hashable, Level};
use orchard::tree::MerkleHashOrchard;

/// Create a deterministic test leaf from an index.
///
/// Produces a valid Pallas field element (32 bytes) that is unique per index.
/// Uses Sinsemilla `combine` at different levels to produce varied hashes.
pub fn test_leaf(index: u64) -> [u8; 32] {
    let empty = MerkleHashOrchard::empty_leaf();
    let varied = MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
    MerkleHashOrchard::combine(Level::from(0), &empty, &varied).to_bytes()
}
