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

/// Build aux key for an epoch blob: e{index} (9 bytes: 'e' + u64 BE).
pub fn epoch_key(index: u64) -> [u8; 9] {
    let mut key = [0u8; 9];
    key[0] = b'e';
    key[1..9].copy_from_slice(&index.to_be_bytes());
    key
}

/// Build aux key for an MMR node: m{pos} (9 bytes: 'm' + u64 BE).
/// Re-exports from grovedb-mmr for consistency.
pub use grovedb_mmr::mmr_node_key;
