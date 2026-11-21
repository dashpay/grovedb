//! A node as a batch holds it between the call that stages it and the
//! application of the batch.

use lignin_format::tree_page::Hash;
use lignin_tree::{hash::Stale, node::NodeHandle};

use crate::txn::{Hosted, LinkPut, NodePut, ValuePut};

/// A link of a staged node: the child by its key, as Merk holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedLink {
    /// The child's key.
    pub key: Box<[u8]>,
    /// The child's hash.
    pub hash: Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The child's aggregate data.
    pub aggregate: Box<[u8]>,
}

impl StagedLink {
    fn of(link: &LinkPut<'_>) -> Self {
        Self {
            key: link.key.into(),
            hash: *link.hash,
            heights: link.heights,
            aggregate: link.aggregate.into(),
        }
    }

    fn as_put(&self) -> LinkPut<'_> {
        LinkPut {
            key: &self.key,
            hash: &self.hash,
            heights: self.heights,
            aggregate: &self.aggregate,
        }
    }
}

/// A node that a batch holds as the outcome of its key: every part of the
/// record that was put, but the Merks the node hosts, which the batch keeps
/// by the element ([`StagedBatch::host`](super::StagedBatch::host)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedNode {
    /// The node's key.
    pub key: Box<[u8]>,
    /// The node's feature type with its aggregate data.
    pub feature: Box<[u8]>,
    /// The node's value hash.
    pub value_hash: Hash,
    /// The node's key-value hash.
    pub kv_hash: Hash,
    /// The link to the left child.
    pub left: Option<StagedLink>,
    /// The link to the right child.
    pub right: Option<StagedLink>,
    /// The node's value, or `None` where the node keeps the value the
    /// transaction has under its key when the batch is applied.
    pub value: Option<Box<[u8]>>,
    /// The hashes of the node that its writer did not make.
    pub stale: Stale,
    /// Where the writer last saw the node.
    pub at: Option<NodeHandle>,
}

impl StagedNode {
    pub(super) fn of(put: &NodePut<'_>) -> Self {
        Self {
            key: put.key.into(),
            feature: put.feature.into(),
            value_hash: *put.value_hash,
            kv_hash: *put.kv_hash,
            left: put.left.as_ref().map(StagedLink::of),
            right: put.right.as_ref().map(StagedLink::of),
            value: match put.value {
                ValuePut::Keep => None,
                ValuePut::Value(value) => Some(value.into()),
            },
            stale: put.stale,
            at: put.at,
        }
    }

    pub(super) fn as_put<'a>(&'a self, hosted: Hosted<'a>) -> NodePut<'a> {
        NodePut {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left: self.left.as_ref().map(StagedLink::as_put),
            right: self.right.as_ref().map(StagedLink::as_put),
            hosted,
            value: match &self.value {
                None => ValuePut::Keep,
                Some(value) => ValuePut::Value(value),
            },
            stale: self.stale,
            at: self.at,
        }
    }
}
