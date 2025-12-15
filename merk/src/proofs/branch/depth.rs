//! Depth calculation utilities for branch queries.
//!
//! This module provides functions for calculating tree depth from element count
//! and for calculating optimal chunk depth splitting.

/// Calculate the depth of a balanced binary tree from its element count.
///
/// For a balanced AVL tree, the minimum depth needed to hold `count` elements
/// is `ceil(log2(count + 1))`.
///
/// # Arguments
/// * `count` - The number of elements in the tree
///
/// # Returns
/// The depth of the tree as a u8
///
/// # Examples
/// ```
/// use grovedb_merk::proofs::branch::depth::calculate_tree_depth_from_count;
///
/// assert_eq!(calculate_tree_depth_from_count(0), 0);
/// assert_eq!(calculate_tree_depth_from_count(1), 1);
/// assert_eq!(calculate_tree_depth_from_count(3), 2);
/// assert_eq!(calculate_tree_depth_from_count(7), 3);
/// assert_eq!(calculate_tree_depth_from_count(15), 4);
/// ```
pub fn calculate_tree_depth_from_count(count: u64) -> u8 {
    if count == 0 {
        return 0;
    }
    // For a balanced tree, depth = ceil(log2(count + 1))
    // We compute this as: number of bits needed to represent count
    // 64 - leading_zeros gives us the position of the highest set bit + 1
    (64 - count.leading_zeros()) as u8
}

/// Calculate optimal chunk depths for even splitting of a tree.
///
/// Instead of naive splitting like `[8, 8, 4]` for tree_depth=20 with
/// max_depth=8, this distributes depths evenly like `[7, 7, 6]`.
///
/// # Arguments
/// * `tree_depth` - Total depth of the tree
/// * `max_depth` - Maximum depth per chunk
///
/// # Returns
/// A vector of chunk depths that sum to `tree_depth`, where each depth is <=
/// `max_depth`
///
/// # Examples
/// ```
/// use grovedb_merk::proofs::branch::depth::calculate_chunk_depths;
///
/// assert_eq!(calculate_chunk_depths(20, 8), vec![7, 7, 6]);
/// assert_eq!(calculate_chunk_depths(15, 5), vec![5, 5, 5]);
/// assert_eq!(calculate_chunk_depths(10, 4), vec![4, 3, 3]);
/// assert_eq!(calculate_chunk_depths(5, 10), vec![5]);
/// ```
pub fn calculate_chunk_depths(tree_depth: u8, max_depth: u8) -> Vec<u8> {
    if tree_depth == 0 {
        return vec![0];
    }

    if tree_depth <= max_depth {
        return vec![tree_depth];
    }

    // Calculate number of chunks needed: ceil(tree_depth / max_depth)
    let num_chunks = ((tree_depth as u32) + (max_depth as u32) - 1) / (max_depth as u32);

    // Calculate base depth per chunk and remainder
    let base_depth = (tree_depth as u32) / num_chunks;
    let remainder = (tree_depth as u32) % num_chunks;

    // Distribute remainder across chunks for even distribution
    // Higher depth chunks come first (they represent higher tree levels)
    (0..num_chunks)
        .map(|i| {
            if i < remainder {
                (base_depth + 1) as u8
            } else {
                base_depth as u8
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_tree_depth_from_count_edge_cases() {
        assert_eq!(calculate_tree_depth_from_count(0), 0);
        assert_eq!(calculate_tree_depth_from_count(1), 1);
    }

    #[test]
    fn test_calculate_tree_depth_from_count_powers_of_two_minus_one() {
        // Perfect binary trees: 2^n - 1 elements fit in n levels
        assert_eq!(calculate_tree_depth_from_count(1), 1); // 2^1 - 1
        assert_eq!(calculate_tree_depth_from_count(3), 2); // 2^2 - 1
        assert_eq!(calculate_tree_depth_from_count(7), 3); // 2^3 - 1
        assert_eq!(calculate_tree_depth_from_count(15), 4); // 2^4 - 1
        assert_eq!(calculate_tree_depth_from_count(31), 5); // 2^5 - 1
        assert_eq!(calculate_tree_depth_from_count(63), 6); // 2^6 - 1
        assert_eq!(calculate_tree_depth_from_count(127), 7); // 2^7 - 1
        assert_eq!(calculate_tree_depth_from_count(255), 8); // 2^8 - 1
    }

    #[test]
    fn test_calculate_tree_depth_from_count_powers_of_two() {
        // One more element than perfect requires one more level
        assert_eq!(calculate_tree_depth_from_count(2), 2);
        assert_eq!(calculate_tree_depth_from_count(4), 3);
        assert_eq!(calculate_tree_depth_from_count(8), 4);
        assert_eq!(calculate_tree_depth_from_count(16), 5);
    }

    #[test]
    fn test_calculate_tree_depth_from_count_large_values() {
        assert_eq!(calculate_tree_depth_from_count(1000), 10);
        assert_eq!(calculate_tree_depth_from_count(1_000_000), 20);
        assert_eq!(calculate_tree_depth_from_count(1_000_000_000), 30);
    }

    #[test]
    fn test_calculate_chunk_depths_no_splitting_needed() {
        assert_eq!(calculate_chunk_depths(5, 10), vec![5]);
        assert_eq!(calculate_chunk_depths(8, 8), vec![8]);
        assert_eq!(calculate_chunk_depths(3, 5), vec![3]);
    }

    #[test]
    fn test_calculate_chunk_depths_even_split() {
        assert_eq!(calculate_chunk_depths(15, 5), vec![5, 5, 5]);
        assert_eq!(calculate_chunk_depths(20, 10), vec![10, 10]);
        assert_eq!(calculate_chunk_depths(12, 4), vec![4, 4, 4]);
    }

    #[test]
    fn test_calculate_chunk_depths_uneven_split() {
        // 20 / 8 = 2.5, so 3 chunks needed
        // 20 / 3 = 6 remainder 2, so [7, 7, 6]
        assert_eq!(calculate_chunk_depths(20, 8), vec![7, 7, 6]);

        // 10 / 4 = 2.5, so 3 chunks needed
        // 10 / 3 = 3 remainder 1, so [4, 3, 3]
        assert_eq!(calculate_chunk_depths(10, 4), vec![4, 3, 3]);

        // 17 / 5 = 3.4, so 4 chunks needed
        // 17 / 4 = 4 remainder 1, so [5, 4, 4, 4]
        assert_eq!(calculate_chunk_depths(17, 5), vec![5, 4, 4, 4]);
    }

    #[test]
    fn test_calculate_chunk_depths_edge_cases() {
        assert_eq!(calculate_chunk_depths(0, 8), vec![0]);
        assert_eq!(calculate_chunk_depths(1, 1), vec![1]);
    }

    #[test]
    fn test_chunk_depths_sum_to_tree_depth() {
        for tree_depth in 1..50u8 {
            for max_depth in 1..20u8 {
                let chunks = calculate_chunk_depths(tree_depth, max_depth);
                let sum: u8 = chunks.iter().sum();
                assert_eq!(
                    sum, tree_depth,
                    "Chunks {:?} should sum to {} for max_depth {}",
                    chunks, tree_depth, max_depth
                );
            }
        }
    }

    #[test]
    fn test_chunk_depths_all_within_max() {
        for tree_depth in 1..50u8 {
            for max_depth in 1..20u8 {
                let chunks = calculate_chunk_depths(tree_depth, max_depth);
                for chunk in &chunks {
                    assert!(
                        *chunk <= max_depth,
                        "Chunk depth {} exceeds max_depth {} for tree_depth {}",
                        chunk,
                        max_depth,
                        tree_depth
                    );
                }
            }
        }
    }
}
