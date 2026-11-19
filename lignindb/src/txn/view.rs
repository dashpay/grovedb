//! A node as a write transaction shows it: a cell of a page, or a record of
//! its own write set.

use lignin_format::{
    tree_page::{CellValue, Hash, Side},
    value_log::{PendingValue, ValuePointer},
};
use lignin_tree::node::{NodeHandle, NodeRef};

use super::write_set::{hash_of, EntryId, Kind, LinkEntry, NodeEntry, WriteSet};

/// The value of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeValue<'a> {
    /// The value's bytes: they are in the node's cell, or the transaction
    /// holds them.
    Bytes(&'a [u8]),
    /// The value is in the value log, at this pointer. It is read with the
    /// node's page as its owner (TREE-08).
    Placed(ValuePointer),
    /// The value is out of line and not in the value log yet, and it is
    /// not one the transaction wrote: its bytes are with the frame of its
    /// page (FL7, TREE-08).
    Pending(PendingValue),
}

impl NodeValue<'_> {
    /// The length of the value in bytes, wherever the value is.
    pub fn len(&self) -> u32 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u32,
            Self::Placed(pointer) => pointer.value_len(),
            Self::Pending(pending) => pending.value_len(),
        }
    }

    /// Whether the value has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A link of a node: what Merk keeps about a child in its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkView<'a> {
    /// The child's hash.
    pub hash: &'a Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The length of the child's key.
    pub child_key_len: u8,
    /// The child's aggregate data.
    pub aggregate: &'a [u8],
    /// The child's key, where the node is a record of the transaction's
    /// write set. A cell names its child by an address, which is followed
    /// through the node's handle ([`Scope::child`](super::Scope::child)).
    pub key: Option<&'a [u8]>,
}

#[derive(Debug, Clone, Copy)]
enum Source<'a> {
    Cell {
        node: NodeRef<'a>,
        /// The bytes of the node's value, if the transaction holds them out
        /// of line.
        held: Option<&'a [u8]>,
    },
    Record {
        set: &'a WriteSet,
        id: EntryId,
    },
}

/// One node as an operation of a write transaction reads it
/// (`transactions.md` RD3, RD5): the cell of the node in the transaction's
/// frames or in a committed page, or, while the transaction serves its
/// records by key (WT7b), the record it wrote.
///
/// It borrows the frame or the write set, which do not change while the
/// operation runs (LT2).
#[derive(Debug, Clone, Copy)]
pub struct NodeView<'a> {
    source: Source<'a>,
}

impl<'a> NodeView<'a> {
    pub(crate) fn cell(node: NodeRef<'a>, held: Option<&'a [u8]>) -> Self {
        Self {
            source: Source::Cell { node, held },
        }
    }

    pub(crate) fn record(set: &'a WriteSet, id: EntryId) -> Self {
        debug_assert!(matches!(set.entry(id).kind, Kind::Node(_)));
        Self {
            source: Source::Record { set, id },
        }
    }

    fn entry(set: &'a WriteSet, id: EntryId) -> NodeEntry {
        match set.entry(id).kind {
            Kind::Node(node) => node,
            _ => unreachable!("a record view is of a written node"),
        }
    }

    /// The node's key.
    pub fn key(&self) -> &'a [u8] {
        match self.source {
            Source::Cell { node, .. } => node.key(),
            Source::Record { set, id } => set.key(id),
        }
    }

    /// The node's feature type with its aggregate data.
    pub fn feature(&self) -> &'a [u8] {
        match self.source {
            Source::Cell { node, .. } => node.feature(),
            Source::Record { set, id } => set.bytes_at(Self::entry(set, id).feature),
        }
    }

    /// The node's value hash.
    pub fn value_hash(&self) -> &'a Hash {
        match self.source {
            Source::Cell { node, .. } => node.value_hash(),
            Source::Record { set, id } => hash_of(set.bytes_at(Self::entry(set, id).value_hash)),
        }
    }

    /// The node's key-value hash.
    pub fn kv_hash(&self) -> &'a Hash {
        match self.source {
            Source::Cell { node, .. } => node.kv_hash(),
            Source::Record { set, id } => hash_of(set.bytes_at(Self::entry(set, id).kv_hash)),
        }
    }

    /// The node's value.
    pub fn value(&self) -> NodeValue<'a> {
        match self.source {
            Source::Cell { node, held } => match (node.value(), held) {
                (CellValue::Inline(value), _) => NodeValue::Bytes(value),
                (_, Some(held)) => NodeValue::Bytes(held),
                (CellValue::OutOfLine(pointer), None) => NodeValue::Placed(pointer),
                (CellValue::Pending(pending), None) => NodeValue::Pending(pending),
            },
            // A record is shown only once the transaction has its own copy
            // of every value its write set refers to (WT7).
            Source::Record { set, id } => NodeValue::Bytes(set.node_value(id).unwrap_or_default()),
        }
    }

    /// The length of the node's value in bytes.
    pub fn value_len(&self) -> u32 {
        match self.source {
            Source::Cell { node, .. } => node.value_len(),
            Source::Record { .. } => self.value().len(),
        }
    }

    /// The link to the child on `side`.
    pub fn link(&self, side: Side) -> Option<LinkView<'a>> {
        match self.source {
            Source::Cell { node, .. } => node.link(side).map(|link| LinkView {
                hash: link.hash,
                heights: link.heights,
                child_key_len: link.child_key_len,
                aggregate: link.aggregate,
                key: None,
            }),
            Source::Record { set, id } => {
                let node = Self::entry(set, id);
                let link: Option<LinkEntry> = match side {
                    Side::Left => node.left,
                    Side::Right => node.right,
                };
                link.map(|link| LinkView {
                    hash: hash_of(set.bytes_at(link.hash)),
                    heights: link.heights,
                    child_key_len: link.key.len() as u8,
                    aggregate: set.bytes_at(link.aggregate),
                    key: Some(set.bytes_at(link.key)),
                })
            }
        }
    }

    /// The node's name, to keep: good until a cell leaves the node's page
    /// or the lease it was read under ends (PN8). A record of the write set
    /// has no cell and so no handle: it is read by its key.
    pub fn handle(&self) -> Option<NodeHandle> {
        match self.source {
            Source::Cell { node, .. } => Some(node.handle()),
            Source::Record { .. } => None,
        }
    }

    /// Whether the node is shown from the transaction's write set, by its
    /// key, and not from a cell.
    pub fn is_record(&self) -> bool {
        matches!(self.source, Source::Record { .. })
    }
}
