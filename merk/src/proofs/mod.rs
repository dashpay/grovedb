pub mod chunk;
pub mod encoding;
pub mod query;
pub mod tree;

pub use encoding::{encode_into, Decoder};
pub use query::Query;
pub use tree::Tree;

use crate::tree::Hash;

/// A proof operator, executed to verify the data in a Merkle proof.
#[derive(Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Represents the hash of a tree node.
    Hash(Hash),

    /// Represents the hash of the key/value pair of a tree node.
    KVHash(Hash),

    /// Represents the key/value_hash pair of a tree node
    KVDigest(Vec<u8>, Hash),

    /// Represents the key and value of a tree node.
    KV(Vec<u8>, Vec<u8>),
}
