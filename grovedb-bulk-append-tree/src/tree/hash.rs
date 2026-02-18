//! Hash helpers for the BulkAppendTree.
//!
//! Each function uses a distinct domain separation tag to prevent
//! cross-domain hash collisions.

/// Update the running buffer hash chain:
/// `blake3("bulk_chain" || prev || blake3(value))`.
pub fn chain_buffer_hash(prev: &[u8; 32], value: &[u8]) -> [u8; 32] {
    let value_hash = blake3::hash(value);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulk_chain");
    hasher.update(prev);
    hasher.update(value_hash.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Compute state_root = `blake3("bulk_state" || mmr_root || buffer_hash)`.
///
/// `total_count` and `chunk_power` are not included here because they are
/// already authenticated by the Merk value hash (they are fields of the
/// serialized Element).
pub fn compute_state_root(mmr_root: &[u8; 32], buffer_hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulk_state");
    hasher.update(mmr_root);
    hasher.update(buffer_hash);
    *hasher.finalize().as_bytes()
}
