//! The nodes of a Merk as a reader finds them: from the root that the
//! subtree's entry names, at the version the reader reads.
//!
//! A reader has the subtree open ([`Opened`]) and reads a node by its key
//! ([`Opened::node`]), or walks the nodes in key order with a cursor that
//! it keeps from one operation to the next ([`NodeWalk`]). Both show a
//! node as a write transaction shows a committed one
//! ([`NodeView`](crate::txn::NodeView)), with the value read through the
//! value log where it is not in the cell.

use std::fmt;

use lignin_format::tree_page::Side;
use lignin_store::{load::LoadBuffers, values::ValueStore};
use lignin_tree::{
    cursor::Cursor,
    node::{NodeReader, PinnedNode},
};

use super::{
    read::Operation,
    records::{Opened, ReadError},
    versions::OperationGuard,
};
use crate::txn::{ChildKeys, NodeView};

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

impl Opened<'_, '_> {
    /// Shows `read` the node under `key` of the subtree's Merk, or `None`
    /// if the version has none: found from the root that the entry names,
    /// by its key (`transactions.md` RD5).
    ///
    /// `children`, where it is given, takes the key of each child of the
    /// node, read from the child's cell.
    pub fn node<R>(
        &self,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
        let reader = self.operation.nodes();
        let mut buffers = LoadBuffers::new();
        let mut cursor = Cursor::open(&reader, self.root());
        let found = cursor.seek(&reader, &mut buffers, key)?;
        let found = found.filter(|node| node.node().key() == key);
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
    /// next, and reads the version it was made at.
    pub fn walk(&self) -> NodeWalk {
        NodeWalk {
            cursor: Cursor::open(&self.operation.nodes(), self.root()),
            buffers: LoadBuffers::new(),
        }
    }
}

/// A cursor over the nodes of one Merk at one version, which a reader
/// keeps from one operation to the next ([`Opened::walk`]).
///
/// It holds no page and no guard between two calls (`lignin_tree::cursor`):
/// each call is given the operation it is made in, which has to be one of
/// the reader the cursor was made by: a snapshot's operations are. A call
/// with another is refused (`CursorError::OtherReader`), and the cursor
/// stays as it was.
pub struct NodeWalk {
    cursor: Cursor,
    buffers: LoadBuffers,
}

impl fmt::Debug for NodeWalk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeWalk")
            .field("at", &self.cursor.at())
            .field("root", &self.cursor.root())
            .field("key", &self.cursor.key())
            .finish_non_exhaustive()
    }
}

macro_rules! moves {
    ($($(#[$doc:meta])* $name:ident($($key:ident)?);)*) => {$(
        $(#[$doc])*
        pub fn $name(
            &mut self,
            operation: &Operation<'_>,
            $($key: &[u8],)?
        ) -> Result<(), ReadError> {
            let reader = operation.nodes();
            self.cursor.$name(&reader, &mut self.buffers $(, $key)?)?;
            Ok(())
        }
    )*};
}

impl NodeWalk {
    /// The key the cursor stands on, if it stands on a node.
    pub fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    moves! {
        /// Goes to the first node whose key is `key` or greater.
        seek(key);
        /// Goes to the last node whose key is `key` or smaller.
        seek_for_prev(key);
        /// Goes to the first node.
        seek_to_first();
        /// Goes to the last node.
        seek_to_last();
        /// Goes to the next node. Past the last one the cursor stands on
        /// none.
        next();
        /// Goes to the node before. Before the first one the cursor stands
        /// on none.
        prev();
    }

    /// Shows `read` the node the cursor stands on, or `None` if it stands
    /// on none. `children` takes the key of each child of the node, read
    /// from the child's cell in the same operation.
    pub fn node_with_children<R>(
        &mut self,
        operation: &Operation<'_>,
        children: &mut ChildKeys,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, ReadError> {
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
