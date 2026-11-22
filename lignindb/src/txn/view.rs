//! A node as a write transaction shows it: a cell of a page, or a record of
//! its own write set.

use std::fmt;

use lignin_format::tree_page::{Hash, Side};
use lignin_store::{cache::PageRef, values::ValueStore};
use lignin_tree::{
    directory::Entry,
    hash::Stale,
    node::{NodeHandle, NodeRef, PinnedNode},
    value::{read_placed, read_value, TreeValues, ValueBuf, ValueSource},
};

use super::{
    error::TxnError,
    key::Subtree,
    unlinked::Unlinked,
    write::HostPut,
    write_set::{hash_of, EntryId, Kind, LinkEntry, NodeEntry, NodeValue, WriteSet},
};

/// What the subtree directory holds of an entered subtree, as an operation
/// of a write transaction reads it ([`lignin_tree::directory`]): the
/// transaction's own enters, drops, clears and roots, and for a subtree it
/// has not written the entry of the latest committed state.
///
/// It borrows what the operation read: an entry of the directory, or the
/// transaction's write set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtreeView<'a> {
    /// The key of the root node of the subtree's Merk, or `None` for a
    /// Merk with no node and for a tree that keeps its data flat.
    pub root_key: Option<&'a [u8]>,
    /// The tree type: the bytes the subtree was entered with.
    pub tree_type: &'a [u8],
    /// The element that hosts the subtree, or `None` for the root of the
    /// grove and for a Merk that stands alone.
    pub host: Option<HostPut<'a>>,
}

impl<'a> SubtreeView<'a> {
    /// The view of a subtree that is entered with no root, of which
    /// nothing else is said.
    pub(crate) const ROOTLESS: SubtreeView<'static> = SubtreeView {
        root_key: None,
        tree_type: &[],
        host: None,
    };

    /// The view of an entry of the directory.
    pub(crate) fn of(entry: &'a Entry) -> Self {
        Self {
            root_key: entry.root().map(|root| root.key),
            tree_type: entry.tree_type(),
            host: entry.host().map(|host| HostPut {
                subtree: Subtree::new(*host.parent),
                key: host.key,
                position: host.position,
            }),
        }
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
    /// write set or an unlinked record. A cell names its child by an
    /// address, which is followed through the node's handle
    /// ([`Scope::child`](super::Scope::child)), or read with the node
    /// under a cursor ([`ChildKeys`]).
    pub key: Option<&'a [u8]>,
}

/// The keys of the children of one cell, read with the node
/// ([`MerkCursor::node_with_children`](super::MerkCursor::node_with_children)).
///
/// A cell names its children by address and holds the length of each
/// child's key (`tree-page.md` TP10): the key itself is in the child's
/// cell. Merk's record of a node names each child by its key, so whoever
/// builds that record reads the two cells too. A record of the write set
/// names its children by key, in its links ([`LinkView::key`]), and no key
/// is read for it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChildKeys {
    keys: [Vec<u8>; 2],
}

impl ChildKeys {
    /// No keys yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The key of the child on `side`. It is empty where the node has no
    /// child there, as the key of a child can be, and where the node's
    /// link holds the key itself: [`NodeView::link`] says which.
    pub fn key(&self, side: Side) -> &[u8] {
        &self.keys[Self::at(side)]
    }

    const fn at(side: Side) -> usize {
        match side {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    /// The key on `side`, emptied, to be read into.
    pub(crate) fn slot(&mut self, side: Side) -> &mut Vec<u8> {
        let key = &mut self.keys[Self::at(side)];
        key.clear();
        key
    }

    pub(crate) fn clear(&mut self) {
        self.keys.iter_mut().for_each(Vec::clear);
    }

    /// Takes the key on `side` out, and leaves it empty.
    pub fn take(&mut self, side: Side) -> Vec<u8> {
        std::mem::take(&mut self.keys[Self::at(side)])
    }
}

/// How the value of a cell is read where it is not in the cell.
#[derive(Clone, Copy)]
enum Values<'a> {
    /// A cell of the transaction's frames: as its scope reads it
    /// ([`TreeValues::read_shown`]). `committed` is the frame the cell is
    /// shown in, if that is a committed page the transaction has not
    /// copied.
    Scope {
        values: &'a TreeValues,
        source: &'a ValueSource<'a>,
        committed: Option<&'a PageRef<'a>>,
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
        /// The value log, for a value that a load put there.
        store: &'a ValueStore,
    },
    Unlinked(Unlinked<'a>),
}

/// One node as an operation of a write transaction reads it
/// (`transactions.md` RD3, RD5): the cell of the node in the transaction's
/// frames or in a committed page, or, while the transaction serves its
/// records by key (WT7b), the record it wrote. A key that no node of the
/// Merk's tree is under can hold an unlinked record, which is shown as the
/// node its writer stated ([`Self::is_unlinked`]).
///
/// It borrows the frame or the write set, which do not change while the
/// operation runs (LT2), or the bytes of the record that was read.
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
            .field("is_unlinked", &self.is_unlinked())
            .finish_non_exhaustive()
    }
}

impl<'a> NodeView<'a> {
    /// A cell of the transaction's frames. `committed` is the frame the
    /// cell was read from, if the page is a committed one that the
    /// transaction has not copied.
    pub(crate) fn framed(
        node: NodeRef<'a>,
        values: &'a TreeValues,
        source: &'a ValueSource<'a>,
        committed: Option<&'a PageRef<'a>>,
        stale: Stale,
    ) -> Self {
        let values = Values::Scope {
            values,
            source,
            committed,
        };
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

    /// A node the transaction wrote, as its write set has it. `store` is
    /// the value log, which holds the value of a node that a load wrote
    /// with its value out of line.
    pub(crate) fn record(set: &'a WriteSet, id: EntryId, store: &'a ValueStore) -> Self {
        debug_assert!(matches!(set.entry(id).kind, Kind::Node(_)));
        Self {
            source: Source::Record { set, id, store },
        }
    }

    /// An unlinked record.
    pub(crate) const fn unlinked(record: Unlinked<'a>) -> Self {
        Self {
            source: Source::Unlinked(record),
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
            Source::Record { set, id, .. } => set.key(id),
            Source::Unlinked(record) => record.key,
        }
    }

    /// The node's feature type with its aggregate data.
    pub fn feature(&self) -> &'a [u8] {
        match self.source {
            Source::Cell { node, .. } => node.feature(),
            Source::Record { set, id, .. } => set.bytes_at(Self::entry(set, id).feature),
            Source::Unlinked(record) => record.feature,
        }
    }

    /// The node's value hash.
    pub fn value_hash(&self) -> &'a Hash {
        match self.source {
            Source::Cell { node, .. } => node.value_hash(),
            Source::Record { set, id, .. } => {
                hash_of(set.bytes_at(Self::entry(set, id).value_hash))
            }
            Source::Unlinked(record) => record.value_hash,
        }
    }

    /// The node's key-value hash.
    pub fn kv_hash(&self) -> &'a Hash {
        match self.source {
            Source::Cell { node, .. } => node.kv_hash(),
            Source::Record { set, id, .. } => hash_of(set.bytes_at(Self::entry(set, id).kv_hash)),
            Source::Unlinked(record) => record.kv_hash,
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
                Values::Scope {
                    values,
                    source,
                    committed,
                } => Ok(values.read_shown(source, node, committed, buf)?),
                Values::Committed { store, node } => Ok(read_value(store, node, buf)?),
            },
            // A record is shown only once the transaction has its own copy
            // of every value its write set refers to (WT7). A value that a
            // load put into the value log is read from there.
            Source::Record { set, id, store } => match set.node_value(id) {
                None => Ok(&[]),
                Some(NodeValue::Bytes(bytes)) => Ok(bytes),
                Some(NodeValue::Stored(stored)) => {
                    Ok(read_placed(store, stored.pointer, stored.owner, buf)?)
                }
            },
            Source::Unlinked(record) => Ok(record.value),
        }
    }

    /// The length of the node's value in bytes. It reads no value.
    pub fn value_len(&self) -> u32 {
        match self.source {
            Source::Cell { node, .. } => node.value_len(),
            Source::Record { set, id, .. } => set.node_value(id).map_or(0, |value| value.len()),
            Source::Unlinked(record) => record.value.len() as u32,
        }
    }

    /// Which of the node's stored hashes are stale: its writer did not make
    /// them, and no evaluation has since (`lignin_tree::hash`). A stale
    /// hash is not read as a hash. None is stale in a committed page.
    pub fn stale(&self) -> Stale {
        match self.source {
            Source::Cell { stale, .. } => stale,
            Source::Record { set, id, .. } => Self::entry(set, id).stale,
            // An unlinked record is in no hash: nothing of it is evaluated.
            Source::Unlinked(_) => Stale::NONE,
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
            Source::Record { set, id, .. } => {
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
            Source::Unlinked(record) => record.link(side).map(|link| LinkView {
                hash: link.hash,
                heights: link.heights,
                child_key_len: link.key.len() as u8,
                aggregate: link.aggregate,
                key: Some(link.key),
            }),
        }
    }

    /// The node's name, to keep: good until a cell leaves the node's page
    /// or the lease it was read under ends (PN8). A record of the write set
    /// has no cell and so no handle, and neither has an unlinked record:
    /// they are read by key.
    pub fn handle(&self) -> Option<NodeHandle> {
        match self.source {
            Source::Cell { node, .. } => Some(node.handle()),
            Source::Record { .. } | Source::Unlinked(_) => None,
        }
    }

    /// Whether the node is an unlinked record: a record under a key that
    /// no node of the Merk's tree is under, kept in the data column of the
    /// subtree and in no hash (`lignindb::txn`, "Unlinked records"). Its
    /// links name their children by key, and a child need not be there.
    pub fn is_unlinked(&self) -> bool {
        matches!(self.source, Source::Unlinked(_))
    }

    /// Whether the node is shown from the transaction's write set, by its
    /// key, and not from a cell.
    pub fn is_record(&self) -> bool {
        matches!(self.source, Source::Record { .. })
    }
}
