//! MMR utility functions.

use ckb_merkle_mountain_range::helper::{get_peaks, pos_height_in_tree};

/// Returns the exact number of Blake3 hash calls for pushing one leaf.
///
/// This is: 1 (leaf hash) + trailing_ones(leaf_count) (merge hashes).
/// Root bagging is NOT included (computed separately when needed).
pub fn hash_count_for_push(leaf_count: u64) -> u32 {
    1 + leaf_count.trailing_ones()
}

/// Derive the number of leaves from mmr_size.
///
/// Uses the ckb helper to count peaks, which correspond to leaf subtree sizes.
pub fn mmr_size_to_leaf_count(mmr_size: u64) -> u64 {
    if mmr_size == 0 {
        return 0;
    }
    // Each peak at height h contains 2^h leaves
    let peaks = get_peaks(mmr_size);
    peaks
        .iter()
        .map(|&peak_pos| {
            let height = pos_height_in_tree(peak_pos);
            1u64 << height
        })
        .sum()
}

#[cfg(test)]
mod tests {
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
