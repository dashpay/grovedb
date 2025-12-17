//! Grove trunk query result for verified trunk chunk proofs.
//!
//! Contains the verified elements from a trunk query as GroveDB Elements,
//! along with leaf keys and their hashes for subsequent branch queries.

use std::collections::BTreeMap;

use grovedb_merk::CryptoHash;

use crate::Element;

/// Result from verifying a trunk chunk proof at the GroveDB level.
///
/// Unlike `TrunkQueryResult` which contains raw proof ops, this struct
/// contains deserialized GroveDB Elements and provides the leaf keys
/// needed for subsequent branch queries.
#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Debug, Clone, PartialEq)]
pub struct GroveTrunkQueryResult {
    /// The elements from the trunk proof, keyed by their key.
    /// These are the deserialized GroveDB Elements from the proof nodes.
    pub elements: BTreeMap<Vec<u8>, Element>,

    /// Leaf nodes (nodes whose children are `Node::Hash` placeholders).
    /// Maps key -> node hash for subsequent branch queries.
    /// The hash is the hash of the node at that key, which should match
    /// the branch_root_hash when verifying a branch proof for that key.
    /// Will be empty if the entire subtree was returned.
    pub leaf_keys: BTreeMap<Vec<u8>, CryptoHash>,

    /// Calculated chunk depths for optimal splitting.
    /// For example, tree_depth=20 with max_depth=8 yields `[7, 7, 6]`
    /// instead of naive `[8, 8, 4]`.
    pub chunk_depths: Vec<u8>,

    /// The calculated total depth of the tree based on element count.
    pub max_tree_depth: u8,
}
