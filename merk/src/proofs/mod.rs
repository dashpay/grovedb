// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Merk proofs

#[cfg(feature = "full")]
pub mod chunk;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod encoding;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod query;
#[cfg(any(feature = "full", feature = "verify"))]
pub mod tree;

#[cfg(feature = "full")]
pub use encoding::encode_into;
#[cfg(any(feature = "full", feature = "verify"))]
pub use encoding::Decoder;
#[cfg(any(feature = "full", feature = "verify"))]
pub use query::Query;
#[cfg(feature = "full")]
pub use tree::Tree;

#[cfg(any(feature = "full", feature = "verify"))]
use crate::{tree::CryptoHash, TreeFeatureType};

#[cfg(any(feature = "full", feature = "verify"))]
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

#[cfg(any(feature = "full", feature = "verify"))]
/// A selected piece of data about a single tree node, to be contained in a
/// `Push` operator in a proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// Represents the hash of a tree node.
    Hash(CryptoHash),

    /// Represents the hash of the key/value pair of a tree node.
    KVHash(CryptoHash),

    /// Represents the key/value_hash pair of a tree node
    KVDigest(Vec<u8>, CryptoHash),

    /// Represents the key and value of a tree node.
    KV(Vec<u8>, Vec<u8>),

    /// Represents the key, value and value_hash of a tree node
    KVValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Represents, the key, value, value_hash and feature_type of a tree node
    KVValueHashFeatureType(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType),

    /// Represents the key, value of some referenced node and value_hash of
    /// current tree node
    KVRefValueHash(Vec<u8>, Vec<u8>, CryptoHash),
}
