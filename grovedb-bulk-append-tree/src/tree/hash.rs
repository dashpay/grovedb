//! Hash helpers for the BulkAppendTree.

/// Update the running buffer hash chain: blake3(prev || blake3(value)).
pub(crate) fn chain_buffer_hash(prev: &[u8; 32], value: &[u8]) -> [u8; 32] {
    let value_hash = blake3::hash(value);
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(prev);
    input[32..].copy_from_slice(value_hash.as_bytes());
    *blake3::hash(&input).as_bytes()
}

/// Compute state_root = blake3(mmr_root || buffer_hash).
pub(crate) fn compute_state_root(mmr_root: &[u8; 32], buffer_hash: &[u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(mmr_root);
    input[32..].copy_from_slice(buffer_hash);
    *blake3::hash(&input).as_bytes()
}
