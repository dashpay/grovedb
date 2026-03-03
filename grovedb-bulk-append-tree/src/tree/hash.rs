//! Hash helpers for the BulkAppendTree.
//!
//! Each function uses a distinct domain separation tag to prevent
//! cross-domain hash collisions.

/// Compute state_root = `blake3("bulk_state" || mmr_root || dense_tree_root)`.
///
/// `total_count` and `height` are not included here because they are
/// already authenticated by the Merk value hash (they are fields of the
/// serialized Element).
pub fn compute_state_root(mmr_root: &[u8; 32], dense_tree_root: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulk_state");
    hasher.update(mmr_root);
    hasher.update(dense_tree_root);
    *hasher.finalize().as_bytes()
}
