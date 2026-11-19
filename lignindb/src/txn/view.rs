//! A node as a write transaction shows it: a cell of a page, or a record of
//! its own write set.

use std::fmt;

use lignin_format::tree_page::{Hash, Side};
use lignin_store::values::ValueStore;
use lignin_tree::{
    hash::Stale,
    node::{NodeHandle, NodeRef, PinnedNode},
    value::{read_value, TreeValues, ValueBuf, ValueSource},
};

use super::{
    error::TxnError,
    write_set::{hash_of, EntryId, Kind, LinkEntry, NodeEntry, WriteSet},
};

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

/// How the value of a cell is read where it is not in the cell.
#[derive(Clone, Copy)]
enum Values<'a> {
    /// A cell of the transaction's frames: as its scope reads it
    /// ([`TreeValues::read`]).
    Scope {
        values: &'a TreeValues,
        source: &'a ValueSource<'a>,
    },
    /// A cell of a committed page, which `node` keeps pinned
    /// ([`read_value`]).
    Committed {
        store: &'a ValueStore,
        node: &'a PinnedNode<'a>,
    },
}

#[derive(Clone, Copy)]
enum Source<'a> {
    Cell {
        node: NodeRef<'a>,
        values: Values<'a>,
        stale: Stale,
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
#[derive(Clone, Copy)]
pub struct NodeView<'a> {
    source: Source<'a>,
}

impl fmt::Debug for NodeView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeView")
            .field("key", &self.key())
            .field("value_len", &self.value_len())
            .field("is_record", &self.is_record())
            .finish_non_exhaustive()
    }
}

impl<'a> NodeView<'a> {
    /// A cell of the transaction's frames.
    pub(crate) fn framed(
        node: NodeRef<'a>,
        values: &'a TreeValues,
        source: &'a ValueSource<'a>,
        stale: Stale,
    ) -> Self {
        let values = Values::Scope { values, source };
        Self {
            source: Source::Cell {
                node,
                values,
                stale,
            },
        }
    }

    /// A cell of a committed page, which `node` keeps pinned.
    pub(crate) fn committed(node: &'a PinnedNode<'a>, store: &'a ValueStore) -> Self {
        Self {
            source: Source::Cell {
                node: node.node(),
                values: Values::Committed { store, node },
                stale: Stale::NONE,
            },
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

    /// The node's value, wherever it is: in the node's cell, in the value
    /// log, with the frame of its page until write-back has placed it, or
    /// with the transaction that wrote it (`lignin_tree::value`, "Where a
    /// value is").
    ///
    /// A value in its cell and a value the transaction holds are returned
    /// as they are. One in the value log is read into `buf` with one
    /// positional read.
    pub fn value<'b>(&self, buf: &'b mut ValueBuf) -> Result<&'b [u8], TxnError>
    where
        'a: 'b,
    {
        match self.source {
            Source::Cell { node, values, .. } => match values {
                Values::Scope { values, source } => Ok(values.read(source, node, buf)?),
                Values::Committed { store, node } => Ok(read_value(store, node, buf)?),
            },
            // A record is shown only once the transaction has its own copy
            // of every value its write set refers to (WT7).
            Source::Record { set, id } => Ok(set.node_value(id).unwrap_or_default()),
        }
    }

    /// The length of the node's value in bytes. It reads no value.
    pub fn value_len(&self) -> u32 {
        match self.source {
            Source::Cell { node, .. } => node.value_len(),
            Source::Record { set, id } => set.node_value(id).map_or(0, |value| value.len() as u32),
        }
    }

    /// Which of the node's stored hashes are stale: its writer did not make
    /// them, and no evaluation has since (`lignin_tree::hash`). A stale
    /// hash is not read as a hash. None is stale in a committed page.
    pub fn stale(&self) -> Stale {
        match self.source {
            Source::Cell { stale, .. } => stale,
            Source::Record { set, id } => Self::entry(set, id).stale,
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
