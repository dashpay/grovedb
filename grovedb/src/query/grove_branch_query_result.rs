//! Grove branch query result for verified branch chunk proofs.
//!
//! Contains the verified elements from a branch query as GroveDB Elements,
//! along with leaf keys and their hashes for subsequent branch queries
//! if further depth remains.

use std::collections::BTreeMap;

use grovedb_merk::CryptoHash;

use crate::Element;

/// Result from verifying a branch chunk proof at the GroveDB level.
///
/// Unlike `BranchQueryResult` which contains raw proof ops, this struct
/// contains deserialized GroveDB Elements and provides the leaf keys
/// needed for subsequent branch queries to explore deeper levels.
#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Debug, Clone, PartialEq)]
pub struct GroveBranchQueryResult {
    /// The elements from the branch proof, keyed by their key.
    /// These are the deserialized GroveDB Elements from the proof nodes.
    pub elements: BTreeMap<Vec<u8>, Element>,

    /// Leaf nodes (nodes whose children are `Node::Hash` placeholders).
    /// Maps key -> node hash for subsequent branch queries.
    /// The hash is the hash of the node at that key, which should match
    /// the branch_root_hash when verifying a branch proof for that key.
    /// Will be empty if the entire subtree was returned.
    pub leaf_keys: BTreeMap<Vec<u8>, CryptoHash>,

    /// The root hash of the branch subtree.
    /// This should match the expected hash from the parent trunk/branch proof.
    pub branch_root_hash: CryptoHash,
}
