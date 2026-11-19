//! The writes of one operation, as the caller states them, and what it is
//! told when they are applied.

use lignin_format::{tree_page::Hash, CellAddress};
use lignin_tree::node::NodeHandle;

use super::key::{Column, Subtree, Unit};

/// A write of a record that is not a Merk node: an aux, roots or meta
/// record, or a flat entry of an append-only tree (`transactions.md` ST1,
/// ST5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordWrite<'a> {
    /// Stores `value` under `key`, in place of any value that is there.
    Put {
        /// The unit of the record.
        unit: Unit,
        /// The record's key.
        key: &'a [u8],
        /// The record's value.
        value: &'a [u8],
    },
    /// Removes the record under `key`. A delete of a key that has no record
    /// is a write all the same (ST5).
    Delete {
        /// The unit of the record.
        unit: Unit,
        /// The record's key.
        key: &'a [u8],
    },
    /// Removes every record of `unit` whose key is at or after `start` and
    /// before `end`, or to the end of the unit with no `end` (ST4).
    DeleteRange {
        /// The unit of the records.
        unit: Unit,
        /// The first key of the range.
        start: &'a [u8],
        /// The key the range ends before.
        end: Option<&'a [u8]>,
    },
}

impl<'a> RecordWrite<'a> {
    /// A put of `value` under `key` in `column` of `subtree`.
    pub const fn put(column: Column, subtree: Subtree, key: &'a [u8], value: &'a [u8]) -> Self {
        Self::Put {
            unit: Unit::new(column, subtree),
            key,
            value,
        }
    }

    /// A delete of `key` in `column` of `subtree`.
    pub const fn delete(column: Column, subtree: Subtree, key: &'a [u8]) -> Self {
        Self::Delete {
            unit: Unit::new(column, subtree),
            key,
        }
    }

    /// The unit the write is in.
    pub const fn unit(&self) -> Unit {
        match *self {
            Self::Put { unit, .. } | Self::Delete { unit, .. } | Self::DeleteRange { unit, .. } => {
                unit
            }
        }
    }
}

/// A link of a written node, as Merk holds it: the child is named by its
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPut<'a> {
    /// The child's key. The child is the node under this key in the same
    /// Merk (RD5).
    pub key: &'a [u8],
    /// The child's hash.
    pub hash: &'a Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The child's aggregate data (`tree-page.md` TP12).
    pub aggregate: &'a [u8],
}

/// The value of a written node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValuePut<'a> {
    /// The value the node has in the transaction's view when the write is
    /// applied: the write changes a hash or a link and leaves the value
    /// (`node-store.md`, the kinds `StructureChanged` and `HashOnly`).
    Keep,
    /// A value that is stored in the node's cell.
    Inline(&'a [u8]),
    /// A value that is too long for its cell. It stays with the
    /// transaction until its commit carries it in the log group (WT4, FL7).
    /// Where a value goes out of line is the caller's rule (TREE-08).
    OutOfLine(&'a [u8]),
}

/// The Merks a written node hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosted<'a> {
    /// The Merks the node hosts already. A new node hosts none.
    ///
    /// It is for a write that leaves every hosted Merk as it is. A batch
    /// that holds the change set of a Merk and writes the node that hosts
    /// it names the Merk in the node ([`Self::Set`]): the pointer in the
    /// host's cell is written with the root it names. Where the cell shows
    /// that this was not done, the batch is refused
    /// ([`BatchError::HostKeeps`](super::BatchError::HostKeeps)). It does
    /// not show it for a Merk that had no root.
    Keep,
    /// The hosted Merks by position: the node's cell points at the root of
    /// each, as that Merk has it once the whole write is applied
    /// (`tree-layout.md` C3, P5).
    Set(&'a [Subtree]),
}

/// One written node of a Merk: the whole record, as Merk hands it to
/// storage (ST6). Every part is stated, and the cell is changed where it
/// differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodePut<'a> {
    /// The node's key: the record key of the write.
    pub key: &'a [u8],
    /// The node's feature type with its aggregate data.
    pub feature: &'a [u8],
    /// The node's value hash.
    pub value_hash: &'a Hash,
    /// The node's key-value hash.
    pub kv_hash: &'a Hash,
    /// The link to the left child.
    pub left: Option<LinkPut<'a>>,
    /// The link to the right child.
    pub right: Option<LinkPut<'a>>,
    /// The Merks the node hosts.
    pub hosted: Hosted<'a>,
    /// The node's value.
    pub value: ValuePut<'a>,
    /// Where the caller last saw the node, if it kept that. It is a hint:
    /// it is followed while it is good, and the node is found by its key
    /// otherwise (PN8).
    ///
    /// A handle is good for the cell it names, and a cell does not say
    /// which Merk it is in. So the handle is one that was read for this
    /// Merk: a good handle of another Merk whose node has the same key
    /// would be followed.
    pub at: Option<NodeHandle>,
}

/// What a write does to the key of a Merk's root node.
///
/// The root key is a part of a record the engine does not read: of the
/// element that hosts the Merk, or of the base root record. So the writer
/// states it, and the engine keeps the Merk's directory entry by it
/// (`transactions.md` ST3, SC2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootPut<'a> {
    /// The root is the node it was.
    Keep,
    /// The node under this key is the root, or the Merk is empty. A root
    /// changes only with a node: a change set that sets it writes or
    /// deletes one
    /// ([`BatchError::RootAlone`](super::BatchError::RootAlone)).
    Set(Option<&'a [u8]>),
}

/// The change set of one Merk in one operation.
///
/// The data column of the Merk's subtree holds its nodes and no flat
/// record (ST1): a batch that writes both there is refused
/// ([`BatchError::NotFlat`](super::BatchError::NotFlat),
/// [`BatchError::NotMerk`](super::BatchError::NotMerk)). Whether the
/// committed state holds flat records under a subtree that had no Merk is
/// not looked up: which of the two a subtree holds is its writer's to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MerkWrite<'a> {
    /// The Merk's subtree.
    pub subtree: Subtree,
    /// The nodes the operation writes, each key once.
    pub nodes: &'a [NodePut<'a>],
    /// The keys of the nodes it deletes. A delete of a key with no node is
    /// a write all the same (ST5).
    pub removed: &'a [&'a [u8]],
    /// The Merk's root after the operation.
    pub root: RootPut<'a>,
    /// Whether the Merk has a host that this operation does not write, so
    /// that the root stays in its page until a later write names it in the
    /// host (`tree-layout.md` T3). Never on existing GroveVersions, where
    /// every write holds the path up to the root of the grove.
    pub anchored: bool,
}

/// The writes of one operation (`transactions.md` section 1): one batch,
/// applied to the transaction as a whole or not at all (WT10), with one
/// first-write version (WT5).
///
/// The Merk change sets are those of every Merk the operation wrote
/// (`tree-layout.md` P5). The records are applied in the order they are
/// given, so a put after a range delete that covers its key stays. A record
/// key is written at most once in a batch, as a storage batch holds one
/// outcome for a key (GM0); which call survives for a key is the rule of
/// the batch above this one (TXN-08).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Batch<'a> {
    /// The writes of flat records.
    pub records: &'a [RecordWrite<'a>],
    /// The change sets of the Merks.
    pub merks: &'a [MerkWrite<'a>],
}

impl Batch<'_> {
    /// Whether the batch writes nothing. Such a batch adds nothing to the
    /// write set (GM0).
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
            && self.merks.iter().all(|merk| {
                merk.nodes.is_empty() && merk.removed.is_empty() && merk.root == RootPut::Keep
            })
    }
}

/// What the application of a batch reports: where the nodes it wrote are
/// (`tree-layout.md` P5, `engine.md` "The batch across subtrees").
///
/// A holder of node handles that takes this in never has to find a node of
/// its own transaction by key (S6). The report is empty while the
/// transaction serves its records by key (WT7b): they have no cells then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// The handles of the written nodes, in the order of the batch: the
    /// nodes of the first Merk, then those of the second, and so on.
    pub nodes: Vec<NodeHandle>,
    /// Every cell the write carried to another page: where it was, and the
    /// node where it is now.
    pub carried: Vec<(CellAddress, NodeHandle)>,
}
