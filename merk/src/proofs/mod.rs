//! Merk proofs

#[cfg(feature = "minimal")]
pub mod chunk;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod encoding;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree;

#[cfg(feature = "minimal")]
pub use encoding::encode_into;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use encoding::Decoder;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query::Query;
#[cfg(feature = "minimal")]
pub use tree::Tree;

#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::{tree::CryptoHash, TreeFeatureType};

#[cfg(any(feature = "minimal", feature = "verify"))]
/// A proof operator, executed to verify the data in a Merkle proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Pushes a node on the stack.
    /// Signifies ascending node keys
    Push(Node),

    /// Pushes a node on the stack
    /// Signifies descending node keys
    PushInverted(Node),

    /// Pops the top stack item as `parent`. Pops the next top stack item as
    /// `child`. Attaches `child` as the left child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    Parent,

    /// Pops the top stack item as `child`. Pops the next top stack item as
    /// `parent`. Attaches `child` as the right child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    Child,

    /// Pops the top stack item as `parent`. Pops the next top stack item as
    /// `child`. Attaches `child` as the right child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    ParentInverted,

    /// Pops the top stack item as `child`. Pops the next top stack item as
    /// `parent`. Attaches `child` as the left child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    ChildInverted,
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// A selected piece of data about a single tree node, to be contained in a
/// `Push` operator in a proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// Represents the hash of a tree node.
    Hash(CryptoHash),

    /// Represents the hash of the key/value pair of a tree node.
    KVHash(CryptoHash),

    /// Represents the key/value_hash pair of a tree node
    /// same as the Node::KV but the value is not required by the proof
    KVDigest(Vec<u8>, CryptoHash),

    /// Represents the key and value of a tree node.
    KV(Vec<u8>, Vec<u8>),

    /// Represents the key, value and value_hash of a tree node
    KVValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Represents, the key, value, value_hash and feature_type of a tree node
    /// Used by Sum trees
    KVValueHashFeatureType(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType),

    /// Represents the key, value of some referenced node and value_hash of
    /// current tree node
    KVRefValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Represents the key, value and count of a tree node
    /// Used by ProvableCountTree
    KVCount(Vec<u8>, Vec<u8>, u64),

    /// Represents the kv hash and count of a tree node
    /// Used by ProvableCountTree
    KVHashCount(CryptoHash, u64),
}

use std::fmt;

#[cfg(any(feature = "minimal", feature = "verify"))]
impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let node_string = match self {
            Node::Hash(hash) => format!("Hash(HASH[{}])", hex::encode(hash)),
            Node::KVHash(kv_hash) => format!("KVHash(HASH[{}])", hex::encode(kv_hash)),
            Node::KV(key, value) => {
                format!("KV({}, {})", hex_to_ascii(key), hex_to_ascii(value))
            }
            Node::KVValueHash(key, value, value_hash) => format!(
                "KVValueHash({}, {}, HASH[{}])",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash)
            ),
            Node::KVDigest(key, value_hash) => format!(
                "KVDigest({}, HASH[{}])",
                hex_to_ascii(key),
                hex::encode(value_hash)
            ),
            Node::KVRefValueHash(key, value, value_hash) => format!(
                "KVRefValueHash({}, {}, HASH[{}])",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash)
            ),
            Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => format!(
                "KVValueHashFeatureType({}, {}, HASH[{}], {:?})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                feature_type
            ),
            Node::KVCount(key, value, count) => format!(
                "KVCount({}, {}, {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                count
            ),
            Node::KVHashCount(kv_hash, count) => {
                format!("KVHashCount(HASH[{}], {})", hex::encode(kv_hash), count)
            }
        };
        write!(f, "{}", node_string)
    }
}

fn hex_to_ascii(hex_value: &[u8]) -> String {
    if hex_value.len() == 1 && hex_value[0] < b"0"[0] {
        hex::encode(hex_value)
    } else {
        String::from_utf8(hex_value.to_vec()).unwrap_or_else(|_| hex::encode(hex_value))
    }
}
