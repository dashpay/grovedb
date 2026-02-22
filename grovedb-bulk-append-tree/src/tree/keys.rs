//! Key layout for BulkAppendTree aux storage.

/// Metadata key in aux storage.
pub const META_KEY: &[u8] = b"M";

/// Build aux key for a buffer entry: b{index} (5 bytes: 'b' + u32 BE).
pub fn buffer_key(index: u32) -> [u8; 5] {
    let mut key = [0u8; 5];
    key[0] = b'b';
    key[1..5].copy_from_slice(&index.to_be_bytes());
    key
}

/// Build aux key for an MMR node: pos as u64 BE (8 bytes).
/// Re-exports from grovedb-merkle-mountain-range for consistency.
pub use grovedb_merkle_mountain_range::mmr_node_key;
