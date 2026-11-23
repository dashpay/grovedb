//! The nodes of a Merk as a reader finds them: from the root that the
//! subtree's entry names, at the version the reader reads.
//!
//! A reader has the subtree open ([`Opened`]) and reads a node by its key
//! ([`Opened::node`]), or walks the nodes in key order with a cursor that
//! it keeps from one operation to the next ([`NodeWalk`]). Both show a
//! node as a write transaction shows a committed one
//! ([`NodeView`](crate::txn::NodeView)), with the value read through the
//! value log where it is not in the cell.
//!
//! Both show the unlinked records of the subtree as a write transaction
//! does (`lignindb::txn`, "Unlinked records"): a read by key that finds no
//! node of the tree finds the record under the key, and a walk shows the
//! records among the nodes, in key order. Whether the version holds any
//! under the subtree is looked up when a read first asks, and the answer
//! is kept with the subtree as it was opened and with what is kept of it
//! and resumed at the same version ([`Kept`](super::Kept)): where there
//! is none, a read that finds no node reads nothing more, and a walk is
//! the cursor over the tree alone.

use std::fmt;

use lignin_format::tree_page::Side;
use lignin_store::{load::LoadBuffers, values::ValueStore};
use lignin_tree::{
    cursor::{Cursor, CursorError},
    node::{NodeReader, PinnedNode},
};

use super::{
    read::Operation,
    records::{Opened, ReadError, RecordWalk},
    versions::OperationGuard,
};
use crate::txn::{unlinked, ChildKeys, Column, FlatKinds, NodeView};

/// Reads the key of each child of `node` from the child's cell
/// (`tree-page.md` TP10).
fn child_keys(
    reader: &NodeReader<'_, OperationGuard<'_>>,
    buffers: &mut LoadBuffers,
    node: &PinnedNode<'_>,
    children: &mut ChildKeys,
) -> Result<(), ReadError> {
    for side in [Side::Left, Side::Right] {
        let key = children.slot(side);
        if let Some(link) = node.node().link(side) {
            let child = reader.node(link.child, buffers)?;
            key.extend_from_slice(child.node().key());
        }
    }
    Ok(())
}

fn show<R>(
    reader: &NodeReader<'_, OperationGuard<'_>>,
    buffers: &mut LoadBuffers,
    values: &ValueStore,
    node: Option<PinnedNode<'_>>,
    children: Option<&mut ChildKeys>,
    read: impl FnOnce(Option<NodeView<'_>>) -> R,
) -> Result<R, ReadError> {
    match (&node, children) {
        (Some(node), Some(children)) => child_keys(reader, buffers, node, children)?,
        (None, Some(children)) => children.clear(),
        (_, None) => {}
    }
    Ok(read(
        node.as_ref().map(|node| NodeView::committed(node, values)),
    ))
}

/// The flat record under `key` of the data column, whose bytes are
/// `record`: as it is stored where `raw` says that its subtree is of a
/// tree that keeps its data as flat records, and otherwise the unlinked
/// record it is, as a node.
fn flat_or_unlinked<'a>(
    key: &'a [u8],
    record: &'a [u8],
    raw: bool,
) -> Result<NodeView<'a>, ReadError> {
    if raw {
        return Ok(NodeView::flat(key, record));
    }
    let record = unlinked::decode(key, record).map_err(|_| ReadError::NotUnlinked)?;
    Ok(NodeView::unlinked(record))
}

impl Opened<'_, '_> {
    /// Shows `read` the node under `key` of the subtree's Merk, or `None`
    /// if the version has none: found from the root that the entry names,
    /// by its key (`transactions.md` RD5), or the unlinked record under
    /// the key where the tree has no node there.
    ///
    /// `children`, where it is given, takes the key of each child of the
    /// node, read from the child's cell. An unlinked record names its
    /// children by key in its links, and `children` takes none.
    pub fn node<R>(
        &self,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
        self.node_of(key, children, false, read)
    }

    /// As [`Self::node`], for a caller whose subtrees can be of trees
    /// that keep their data as flat records and no Merk: where `flat`
    /// says so of the tree type in the subtree's entry, a flat record
    /// under `key` is shown as it is stored
    /// ([`NodeView::flat_record`]), and not as an unlinked record.
    pub fn node_by_kind<R>(
        &self,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        flat: FlatKinds,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
        self.node_of(key, children, flat(self.entry().tree_type()), read)
    }

    fn node_of<R>(
        &self,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        raw: bool,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
        let reader = self.operation.nodes();
        let mut buffers = LoadBuffers::new();
        let mut cursor = Cursor::open(&reader, self.root());
        let found = cursor.seek(&reader, &mut buffers, key)?;
        let found = found.filter(|node| node.node().key() == key);
        if found.is_none() && self.has_unlinked()? {
            let mut record = Vec::new();
            if self.get(Column::Data, key, &mut record)? {
                if let Some(children) = children {
                    children.clear();
                }
                return Ok(read(Some(flat_or_unlinked(key, &record, raw)?)));
            }
        }
        show(
            &reader,
            &mut buffers,
            self.records.values,
            found,
            children,
            read,
        )
    }

    /// A cursor over the nodes of the subtree's Merk, in key order, not
    /// positioned. It is kept from one operation of the reader to the
    /// next, and reads the version it was made at. Whether that version
    /// holds unlinked records under the subtree is looked up here, if no
    /// read has asked yet.
    pub fn walk(&self) -> Result<NodeWalk, ReadError> {
        self.walk_of(false)
    }

    /// As [`Self::walk`], for a caller whose subtrees can be of trees
    /// that keep their data as flat records and no Merk
    /// ([`Self::node_by_kind`]): the walk shows the flat records of such
    /// a subtree as they are stored.
    pub fn walk_by_kind(&self, flat: FlatKinds) -> Result<NodeWalk, ReadError> {
        self.walk_of(flat(self.entry().tree_type()))
    }

    fn walk_of(&self, raw: bool) -> Result<NodeWalk, ReadError> {
        let unlinked = self.has_unlinked()?.then(|| Unlinked {
            records: self.record_walk(Column::Data),
            back: false,
            lost: false,
        });
        Ok(NodeWalk {
            cursor: Cursor::open(&self.operation.nodes(), self.root()),
            buffers: LoadBuffers::new(),
            unlinked,
            raw,
        })
    }
}

/// The unlinked records of a walk's subtree, in key order: the flat
/// records of the data column under the subtree.
struct Unlinked {
    records: RecordWalk,
    /// Whether the walk was last moved backwards: its two cursors then
    /// stand each at its last key at or before the place, and otherwise
    /// each at its first key at or after it.
    back: bool,
    /// Whether a move of one of the two cursors failed since the walk was
    /// last sought: the two then stand in no place together.
    lost: bool,
}

/// A cursor over the nodes of one Merk at one version, which a reader
/// keeps from one operation to the next ([`Opened::walk`]).
///
/// It holds no page and no guard between two calls (`lignin_tree::cursor`):
/// each call is given the operation it is made in, which has to be one of
/// the reader the cursor was made by: a snapshot's operations are. A call
/// with another is refused (`CursorError::OtherReader`), and the cursor
/// stays as it was.
///
/// Where the subtree holds unlinked records at the walk's version, they
/// are shown among the nodes, in key order: the walk is then two cursors,
/// one over the tree and one over the records, and stands on the nearer of
/// their keys. A call with an operation of another reader is then refused
/// as above before either of the two moves, and after a move that fails
/// in one of them the walk stands on no key until it is sought again.
pub struct NodeWalk {
    cursor: Cursor,
    buffers: LoadBuffers,
    unlinked: Option<Unlinked>,
    /// Whether the subtree is of a tree that keeps its data as flat
    /// records, for a walk that was told which are
    /// ([`Opened::walk_by_kind`]).
    raw: bool,
}

impl fmt::Debug for NodeWalk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeWalk")
            .field("at", &self.cursor.at())
            .field("root", &self.cursor.root())
            .field("key", &self.key())
            .finish_non_exhaustive()
    }
}

macro_rules! seeks {
    ($($(#[$doc:meta])* $name:ident($($key:ident)?), $back:literal;)*) => {$(
        $(#[$doc])*
        pub fn $name(
            &mut self,
            operation: &Operation<'_>,
            $($key: &[u8],)?
        ) -> Result<(), ReadError> {
            self.of(operation)?;
            let reader = operation.nodes();
            let sought = self.cursor.$name(&reader, &mut self.buffers $(, $key)?);
            let sought = sought.map(drop).map_err(ReadError::from);
            let sought = sought.and_then(|()| match &mut self.unlinked {
                None => Ok(()),
                Some(unlinked) => {
                    unlinked.back = $back;
                    unlinked.records.$name(operation $(, $key)?)
                }
            });
            self.moved(sought)
        }
    )*};
}

impl NodeWalk {
    /// Refuses an operation that one of the two cursors would refuse,
    /// before either moves: the cursor over the records alone would take
    /// one of another reader at the same version. A walk over the tree
    /// alone leaves it to the tree's cursor.
    fn of(&self, operation: &Operation<'_>) -> Result<(), ReadError> {
        let Some(unlinked) = &self.unlinked else {
            return Ok(());
        };
        let reader = operation.nodes();
        let opened = (self.cursor.at(), self.cursor.lease());
        if (reader.at(), reader.lease()) != opened {
            return Err(ReadError::Cursor(CursorError::OtherReader {
                opened_at: opened.0,
                opened_under: opened.1,
                given_at: reader.at(),
                given_under: reader.lease(),
            }));
        }
        unlinked.records.records_of(operation).map(drop)
    }

    /// Takes in how a move of the two cursors went: where one of them
    /// failed, the other may have moved, and the walk stands on no key
    /// until it is sought again.
    fn moved(&mut self, moved: Result<(), ReadError>) -> Result<(), ReadError> {
        if let Some(unlinked) = &mut self.unlinked {
            unlinked.lost = moved.is_err();
        }
        moved
    }

    /// Has the next move of the cursor over the unlinked records fail. It
    /// is for the tests, which have no page that fails to load.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn fail_next_record_move(&mut self) {
        if let Some(unlinked) = &mut self.unlinked {
            unlinked.records.failing = true;
        }
    }

    /// Whether a move of one of the two cursors failed since the walk was
    /// last sought.
    fn lost(&self) -> bool {
        (self.unlinked.as_ref()).is_some_and(|unlinked| unlinked.lost)
    }

    /// Which of the two cursors stand on the walk's key: the one over the
    /// tree, and the one over the unlinked records.
    fn on_key(&self) -> (bool, bool) {
        match &self.unlinked {
            None => (self.cursor.key().is_some(), false),
            Some(Unlinked { lost: true, .. }) => (false, false),
            Some(Unlinked { records, back, .. }) => {
                unlinked::nearer(self.cursor.key(), records.key(), *back)
            }
        }
    }

    /// Has both cursors stand as a move in the direction `back` leaves
    /// them, at the walk's key: for a step that goes the other way than
    /// the walk was last moved.
    fn turn(&mut self, operation: &Operation<'_>, back: bool) -> Result<(), ReadError> {
        let turned = (self.unlinked.as_ref()).is_some_and(|unlinked| unlinked.back != back);
        if turned && let Some(key) = self.key().map(<[u8]>::to_vec) {
            match back {
                false => self.seek(operation, &key)?,
                true => self.seek_for_prev(operation, &key)?,
            }
        }
        Ok(())
    }

    /// Steps the cursors that stand on the walk's key: forwards, or
    /// backwards if `back` says so.
    fn step(&mut self, operation: &Operation<'_>, back: bool) -> Result<(), ReadError> {
        self.turn(operation, back)?;
        // A walk over the tree alone is the tree's cursor.
        let (node, record) = match &self.unlinked {
            None => (true, false),
            Some(_) => self.on_key(),
        };
        if node {
            let reader = operation.nodes();
            match back {
                false => self.cursor.next(&reader, &mut self.buffers).map(drop),
                true => self.cursor.prev(&reader, &mut self.buffers).map(drop),
            }?;
        }
        match (record, &mut self.unlinked, back) {
            (true, Some(unlinked), false) => unlinked.records.next(operation),
            (true, Some(unlinked), true) => unlinked.records.prev(operation),
            _ => Ok(()),
        }
    }

    /// The key the cursor stands on, if it stands on a node or on an
    /// unlinked record.
    pub fn key(&self) -> Option<&[u8]> {
        match (self.on_key(), &self.unlinked) {
            ((true, _), _) => self.cursor.key(),
            ((false, true), Some(unlinked)) => unlinked.records.key(),
            _ => None,
        }
    }

    seeks! {
        /// Goes to the first node whose key is `key` or greater.
        seek(key), false;
        /// Goes to the last node whose key is `key` or smaller.
        seek_for_prev(key), true;
        /// Goes to the first node.
        seek_to_first(), false;
        /// Goes to the last node.
        seek_to_last(), true;
    }

    /// Goes to the next node. Past the last one the cursor stands on
    /// none, and a walk that stands on none stays there.
    pub fn next(&mut self, operation: &Operation<'_>) -> Result<(), ReadError> {
        self.of(operation)?;
        if self.lost() {
            return Ok(());
        }
        let stepped = self.step(operation, false);
        self.moved(stepped)
    }

    /// Goes to the node before. Before the first one the cursor stands
    /// on none, and a walk that stands on none stays there.
    pub fn prev(&mut self, operation: &Operation<'_>) -> Result<(), ReadError> {
        self.of(operation)?;
        if self.lost() {
            return Ok(());
        }
        let stepped = self.step(operation, true);
        self.moved(stepped)
    }

    /// Shows `read` the node the cursor stands on, or `None` if it stands
    /// on none. `children` takes the key of each child of the node, read
    /// from the child's cell in the same operation, and none for an
    /// unlinked record, whose links name its children by key.
    pub fn node_with_children<R>(
        &mut self,
        operation: &Operation<'_>,
        children: &mut ChildKeys,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
        self.of(operation)?;
        if self.lost() {
            children.clear();
            return Ok(read(None));
        }
        if let ((false, true), Some(unlinked)) = (self.on_key(), &self.unlinked)
            && let Some(key) = unlinked.records.key()
        {
            children.clear();
            let mut record = Vec::new();
            return match unlinked.records.value(operation, &mut record)? {
                true => Ok(read(Some(flat_or_unlinked(key, &record, self.raw)?))),
                false => Ok(read(None)),
            };
        }
        let reader = operation.nodes();
        let values = operation.records.ok_or(ReadError::NoRecords)?.values;
        let node = self.cursor.node(&reader, &mut self.buffers)?;
        show(
            &reader,
            &mut self.buffers,
            values,
            node,
            Some(children),
            read,
        )
    }
}
