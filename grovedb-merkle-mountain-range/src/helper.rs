/// Convert a 0-based leaf index to its MMR position.
///
/// # Safety (arithmetic)
///
/// Overflows when `index >= 2^63 - 1`. Callers must validate indices
/// before calling (e.g. check `index < mmr_size_to_leaf_count(mmr_size)`).
pub fn leaf_index_to_pos(index: u64) -> u64 {
    // mmr_size - H - 1, H is the height(intervals) of last peak
    leaf_index_to_mmr_size(index) - (index + 1).trailing_zeros() as u64 - 1
}

/// Compute the MMR size after inserting `index + 1` leaves.
///
/// # Safety (arithmetic)
///
/// Overflows when `index >= 2^63 - 1` because `2 * leaves_count` exceeds
/// `u64::MAX`. Callers must validate indices before calling.
pub fn leaf_index_to_mmr_size(index: u64) -> u64 {
    // leaf index start with 0
    let leaves_count = index + 1;

    // the peak count(k) is actually the count of 1 in leaves count's binary
    // representation
    let peak_count = leaves_count.count_ones() as u64;

    2 * leaves_count - peak_count
}

/// Return the height of the subtree rooted at `pos` in the MMR.
///
/// Leaf positions have height 0; internal nodes have height > 0.
pub fn pos_height_in_tree(mut pos: u64) -> u8 {
    if pos == 0 {
        return 0;
    }

    let mut peak_size = u64::MAX >> pos.leading_zeros();
    while peak_size > 0 {
        if pos >= peak_size {
            pos -= peak_size;
        }
        peak_size >>= 1;
    }
    pos as u8
}

/// Offset from a node to its parent at the given height.
pub fn parent_offset(height: u8) -> u64 {
    2 << height
}

/// Offset from a node to its sibling at the given height.
pub fn sibling_offset(height: u8) -> u64 {
    (2 << height) - 1
}

/// Returns the height of the peaks in the mmr, presented by a bitmap.
/// for example, for a mmr with 11 leaves, the mmr_size is 19, it will return
/// 0b1011. 0b1011 indicates that the left peaks are at height 0, 1 and 3.
///           14
///        /       \
///      6          13
///    /   \       /   \
///   2     5     9     12     17
///  / \   /  \  / \   /  \   /  \
/// 0   1 3   4 7   8 10  11 15  16 18
///
/// please note that when the mmr_size is invalid, it will return the bitmap of
/// the last valid mmr. in the below example, the mmr_size is 6, but it's not a
/// valid mmr, it will return 0b11.   2     5
///  / \   /  \
/// 0   1 3   4
pub fn get_peak_map(mmr_size: u64) -> u64 {
    if mmr_size == 0 {
        return 0;
    }

    let mut pos = mmr_size;
    let mut peak_size = u64::MAX >> pos.leading_zeros();
    let mut peak_map = 0;
    while peak_size > 0 {
        peak_map <<= 1;
        if pos >= peak_size {
            pos -= peak_size;
            peak_map |= 1;
        }
        peak_size >>= 1;
    }

    peak_map
}

/// Returns the pos of the peaks in the mmr.
/// for example, for a mmr with 11 leaves, the mmr_size is 19, it will return
/// [14, 17, 18].           14
///        /       \
///      6          13
///    /   \       /   \
///   2     5     9     12     17
///  / \   /  \  / \   /  \   /  \
/// 0   1 3   4 7   8 10  11 15  16 18
///
/// please note that when the mmr_size is invalid, it will return the peaks of
/// the last valid mmr. in the below example, the mmr_size is 6, but it's not a
/// valid mmr (size 4 is the last valid one with 3 leaves), so it will return
/// [2, 3].
///   2
///  / \
/// 0   1 3
pub fn get_peaks(mmr_size: u64) -> Vec<u64> {
    if mmr_size == 0 {
        return vec![];
    }

    let leading_zeros = mmr_size.leading_zeros();
    let mut pos = mmr_size;
    let mut peak_size = u64::MAX >> leading_zeros;
    let mut peaks = Vec::with_capacity(64 - leading_zeros as usize);
    let mut peaks_sum = 0;
    while peak_size > 0 {
        if pos >= peak_size {
            pos -= peak_size;
            peaks.push(peaks_sum + peak_size - 1);
            peaks_sum += peak_size;
        }
        peak_size >>= 1;
    }
    peaks
}

// ── Storage and cost helpers ────────────────────────────────────────────

/// Controls the byte width of MMR storage keys.
///
/// `U64` (default) uses 8-byte big-endian keys for full u64 positions.
/// `U32` uses 4-byte big-endian keys, truncating the position to u32 and
/// saving 4 bytes per key for trees that will never exceed ~4 billion nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MmrKeySize {
    /// 4-byte keys (position truncated to u32).
    U32,
    /// 8-byte keys (full u64 position).
    #[default]
    U64,
}

/// A compact, inline storage key for an MMR node.
///
/// Holds up to 8 bytes and exposes only the relevant prefix via
/// `AsRef<[u8]>`, avoiding heap allocation.
#[derive(Debug)]
pub struct MmrKey {
    bytes: [u8; 8],
    len: usize,
}

impl AsRef<[u8]> for MmrKey {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Build the storage key for an MMR node at a given position.
///
/// Format: raw u64 big-endian (8 bytes).
/// No prefix is needed because each MMR subtree gets its own isolated
/// storage context in GroveDB (keyed by the Blake3 hash of the path).
pub fn mmr_node_key(pos: u64) -> [u8; 8] {
    pos.to_be_bytes()
}

/// Maximum MMR position allowed with [`MmrKeySize::U32`].
///
/// Since we set the MSB for namespace separation (`pos | 0x8000_0000`),
/// positions at or above `2^31` would alias lower positions. This constant
/// is the exclusive upper bound.
pub const MAX_U32_MMR_POSITION: u64 = 1u64 << 31;

/// Build the storage key for an MMR node with configurable key width.
///
/// With [`MmrKeySize::U64`], produces an 8-byte big-endian key with the
/// MSB set. With [`MmrKeySize::U32`], produces a 4-byte key (position
/// truncated to `u32`) with the MSB set.
///
/// The MSB is always set so that MMR keys are namespaced away from other
/// data in the same storage context (e.g. dense tree keys which are
/// 2-byte `u16` positions and never have the MSB of a 4/8-byte key set).
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if `key_size` is `U32` and
/// `pos >= 2^31`, since the MSB tagging would cause position aliasing.
pub fn mmr_node_key_sized(pos: u64, key_size: MmrKeySize) -> crate::Result<MmrKey> {
    match key_size {
        MmrKeySize::U32 => {
            if pos >= MAX_U32_MMR_POSITION {
                return Err(crate::Error::InvalidInput(format!(
                    "MMR position {} exceeds U32 key limit (max {})",
                    pos,
                    MAX_U32_MMR_POSITION - 1
                )));
            }
            let tagged = (pos as u32) | 0x8000_0000;
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(&tagged.to_be_bytes());
            Ok(MmrKey { bytes, len: 4 })
        }
        MmrKeySize::U64 => {
            let tagged = pos | 0x8000_0000_0000_0000;
            Ok(MmrKey {
                bytes: tagged.to_be_bytes(),
                len: 8,
            })
        }
    }
}

/// Returns the exact number of Blake3 hash calls for pushing one leaf.
///
/// This is: 1 (leaf hash) + trailing_ones(leaf_count) (merge hashes).
/// Root bagging is NOT included (computed separately when needed).
pub fn hash_count_for_push(leaf_count: u64) -> u32 {
    1 + leaf_count.trailing_ones()
}

/// Derive the number of leaves from mmr_size.
///
/// The peak map bitmap encodes one bit per peak at height `h`, so its
/// numeric value equals the total leaf count: `sum(2^h)` for each set
/// bit `h`.
pub fn mmr_size_to_leaf_count(mmr_size: u64) -> u64 {
    get_peak_map(mmr_size)
}

#[cfg(test)]
mod grove_util_tests {
    use super::*;

    #[test]
    fn test_hash_count_for_push() {
        assert_eq!(hash_count_for_push(0), 1);
        assert_eq!(hash_count_for_push(1), 2);
        assert_eq!(hash_count_for_push(2), 1);
        assert_eq!(hash_count_for_push(3), 3);
        assert_eq!(hash_count_for_push(4), 1);
        assert_eq!(hash_count_for_push(7), 4);
    }

    #[test]
    fn test_mmr_size_to_leaf_count() {
        assert_eq!(mmr_size_to_leaf_count(0), 0);
        assert_eq!(mmr_size_to_leaf_count(1), 1);
        assert_eq!(mmr_size_to_leaf_count(3), 2);
        assert_eq!(mmr_size_to_leaf_count(4), 3);
        assert_eq!(mmr_size_to_leaf_count(7), 4);
    }

    #[test]
    fn test_u32_key_at_max_valid_position() {
        // Position 2^31 - 1 is the last valid U32 position
        let key = mmr_node_key_sized(MAX_U32_MMR_POSITION - 1, MmrKeySize::U32)
            .expect("max valid position should succeed");
        assert_eq!(key.as_ref().len(), 4);
        // MSB should be set: 0x7FFF_FFFF | 0x8000_0000 = 0xFFFF_FFFF
        assert_eq!(key.as_ref(), &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_u32_key_at_overflow_position() {
        // Position 2^31 would alias position 0 after MSB tagging
        let err = mmr_node_key_sized(MAX_U32_MMR_POSITION, MmrKeySize::U32)
            .expect_err("should reject position >= 2^31");
        assert!(
            matches!(err, crate::Error::InvalidInput(_)),
            "expected InvalidInput, got {:?}",
            err
        );
    }

    #[test]
    fn test_u64_key_allows_large_positions() {
        // U64 keys should work with very large positions
        let key = mmr_node_key_sized(u64::MAX >> 1, MmrKeySize::U64)
            .expect("large U64 position should succeed");
        assert_eq!(key.as_ref().len(), 8);
        // MSB should be set
        assert!(key.as_ref()[0] >= 0x80);
    }
}
