//! Path trunk chunk query for retrieving chunked proofs of tree trunks.
//!
//! A trunk chunk query retrieves the top N levels of a tree at a given path,
//! returning a proof structure that can be verified against the root hash.
//! This is useful for splitting large tree proofs into manageable chunks.

use bincode::{Decode, Encode};

/// Path trunk chunk query
///
/// Represents a path to a specific GroveDB tree and parameters for retrieving
/// a trunk chunk proof from that tree.
///
/// # Requirements
/// The tree at the specified path must support count operations (CountTree,
/// CountSumTree, or ProvableCountTree).
#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct PathTrunkChunkQuery {
    /// Path to the tree to query
    pub path: Vec<Vec<u8>>,
    /// Maximum depth per chunk (determines how the tree is split)
    pub max_depth: u8,
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl PathTrunkChunkQuery {
    /// Create a new path trunk chunk query
    ///
    /// # Arguments
    /// * `path` - Path to the tree to query
    /// * `max_depth` - Maximum depth per chunk (minimum 1)
    pub fn new(path: Vec<Vec<u8>>, max_depth: u8) -> Self {
        Self {
            path,
            max_depth: max_depth.max(1),
        }
    }

    /// Create a new path trunk chunk query from a slice path
    pub fn new_from_slice_path(path: &[&[u8]], max_depth: u8) -> Self {
        Self::new(path.iter().map(|p| p.to_vec()).collect(), max_depth)
    }

    /// Get the path as a slice of slices
    pub fn path_slices(&self) -> Vec<&[u8]> {
        self.path.iter().map(|p| p.as_slice()).collect()
    }
}
