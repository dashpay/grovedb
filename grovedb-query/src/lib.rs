//! Query primitives for GroveDB
//!
//! This crate provides the core query types (Query, QueryItem, SubqueryBranch)
//! used throughout GroveDB for specifying which keys and ranges to include in
//! proofs and query results.

#![warn(missing_docs)]

/// Error types for query operations.
pub mod error;

mod common_path;

mod insert;

mod merge;

/// Query item types representing keys and ranges for tree queries.
pub mod query_item;

mod proof_items;

mod proof_status;

/// Proof primitives: Op, Node, encoding, and TreeFeatureType.
pub mod proofs;

mod query;

mod subquery_branch;

pub use proof_items::ProofItems;
pub use proof_status::ProofStatus;
pub use query::Query;
pub use query_item::{intersect::QueryItemIntersectionResult, QueryItem};
pub use subquery_branch::SubqueryBranch;

/// Type alias for a path.
pub type Path = Vec<Vec<u8>>;

/// Type alias for a Key.
pub type Key = Vec<u8>;

/// Type alias for path-key common pattern.
pub type PathKey = (Path, Key);

/// Parameters controlling proof generation behavior.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProofParams {
    /// Whether to traverse the tree from left to right (`true`) or right to
    /// left (`false`).
    pub left_to_right: bool,
}

/// Convert a byte slice to an ASCII string if all characters are allowed,
/// otherwise hex-encode it.
pub fn hex_to_ascii(hex_value: &[u8]) -> String {
    const ALLOWED_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                                  abcdefghijklmnopqrstuvwxyz\
                                  0123456789_-/\\[]@";

    if hex_value.iter().all(|&c| ALLOWED_CHARS.contains(&c)) {
        String::from_utf8(hex_value.to_vec())
            .unwrap_or_else(|_| format!("0x{}", hex::encode(hex_value)))
    } else {
        format!("0x{}", hex::encode(hex_value))
    }
}
