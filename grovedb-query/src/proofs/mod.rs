//! Proof primitives: Op, Node, encoding, and TreeFeatureType.
//!
//! These types define the proof operators and node variants used to construct
//! and verify Merkle proofs in GroveDB/Merk.

mod encoding;

mod tree_feature_type;

pub use encoding::{encode_into, Decoder};
pub use tree_feature_type::{NodeType, TreeFeatureType};

use crate::hex_to_ascii;

/// The length of a `Hash` (in bytes).
pub const HASH_LENGTH: usize = 32;

/// A zero-filled `Hash`.
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];

/// A cryptographic hash digest.
pub type CryptoHash = [u8; HASH_LENGTH];

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

/// A selected piece of data about a single tree node, to be contained in a
/// `Push` operator in a proof.
///
/// Each variant carries different amounts of information, allowing proofs to
/// include only what's necessary for verification while minimizing proof size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// The node hash only. Used for sibling/cousin nodes not on the query path.
    ///
    /// Contains: `node_hash`
    Hash(CryptoHash),

    /// The key-value hash only. Used for non-queried nodes on the path.
    ///
    /// Contains: `kv_hash` (hash of key concatenated with value_hash)
    KVHash(CryptoHash),

    /// Key and value_hash. Used for proving key existence/absence at
    /// boundaries.
    ///
    /// Contains: `(key, value_hash)`
    KVDigest(Vec<u8>, CryptoHash),

    /// Key and value. Suitable for items where value_hash = H(value).
    ///
    /// Contains: `(key, value)`
    KV(Vec<u8>, Vec<u8>),

    /// Key, value, and value_hash. Standard node for queried items.
    ///
    /// Contains: `(key, value, value_hash)`
    KVValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Key, value, value_hash, and feature type. For trees with special
    /// features.
    ///
    /// Contains: `(key, value, value_hash, feature_type)`
    KVValueHashFeatureType(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType),

    /// Key, referenced value, and hash of serialized Reference element.
    /// For GroveDB references.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash)`
    KVRefValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Key, value, and count. For queried Items in ProvableCountTree.
    ///
    /// Contains: `(key, value, count)`
    KVCount(Vec<u8>, Vec<u8>, u64),

    /// KV hash and count. For non-queried nodes in ProvableCountTree.
    ///
    /// Contains: `(kv_hash, count)`
    KVHashCount(CryptoHash, u64),

    /// Key, referenced value, reference element hash, and count.
    /// For queried References in ProvableCountTree.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash, count)`
    KVRefValueHashCount(Vec<u8>, Vec<u8>, CryptoHash, u64),

    /// Key, value_hash, and count. For proving absence in ProvableCountTree.
    ///
    /// Contains: `(key, value_hash, count)`
    KVDigestCount(Vec<u8>, CryptoHash, u64),
}

use std::fmt;

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
            Node::KVRefValueHashCount(key, value, value_hash, count) => format!(
                "KVRefValueHashCount({}, {}, HASH[{}], {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                count
            ),
            Node::KVDigestCount(key, value_hash, count) => format!(
                "KVDigestCount({}, HASH[{}], {})",
                hex_to_ascii(key),
                hex::encode(value_hash),
                count
            ),
        };
        write!(f, "{}", node_string)
    }
}
