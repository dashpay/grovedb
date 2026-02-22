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

/// Build the storage key for an MMR node at a given position.
///
/// Format: raw u64 big-endian (8 bytes).
/// No prefix is needed because each MMR subtree gets its own isolated
/// storage context in GroveDB (keyed by the Blake3 hash of the path).
pub fn mmr_node_key(pos: u64) -> [u8; 8] {
    pos.to_be_bytes()
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
}
