//! Reading the nodes of a tree page in a frame.

use std::iter::FusedIterator;

use lignin_format::{
    tree_page::{
        CellValue, Cells, Hash, HashField, HostedRoots, Side, TreeCell, TreeLink, TreePage,
    },
    CellAddress, PageId, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::{
    error::NodeError,
    handle::{Lease, NodeHandle},
};

/// The nodes of one tree page, borrowed from the frame that holds it.
///
/// The frame is whatever keeps the page's bytes still while the view lives:
/// a pinned page version, or the latch of a private copy. No accessor
/// allocates.
#[derive(Debug, Clone, Copy)]
pub struct NodePage<'a> {
    page: TreePage<'a>,
    lease: Lease,
}

impl<'a> NodePage<'a> {
    /// The tree page at the start of `frame`, read under `lease`.
    ///
    /// The frame holds a page that was checked when it was loaded, or that
    /// was only changed through [`NodePageMut`](super::NodePageMut). Only
    /// its header is read here, in constant time. A frame that holds a flat
    /// page, or no page, is refused with [`NodeError::Page`].
    pub fn open(frame: &'a [u8], lease: Lease) -> Result<Self, NodeError> {
        let page = TreePage::from_validated(frame, FRAME_FORMAT)?;
        Ok(Self { page, lease })
    }

    pub(super) const fn of(page: TreePage<'a>, lease: Lease) -> Self {
        Self { page, lease }
    }

    /// The page's id.
    pub const fn id(&self) -> PageId {
        self.page.id()
    }

    /// How many cells have left the page. A handle into the page is good
    /// while this is the count it was issued at.
    pub const fn departures(&self) -> u64 {
        self.page.departures()
    }

    /// The lease the page is read under.
    pub const fn lease(&self) -> Lease {
        self.lease
    }

    /// How many nodes the page holds.
    pub const fn len(&self) -> usize {
        self.page.cell_count()
    }

    /// Whether the page holds no node.
    pub const fn is_empty(&self) -> bool {
        self.page.is_empty()
    }

    /// The page as the page codec reads it: its sizes, its fragment roots,
    /// its pending values.
    pub const fn tree_page(&self) -> TreePage<'a> {
        self.page
    }

    /// The node in `slot`, or `None` if the slot is free.
    ///
    /// This is how an address that a page stores is followed: the link of a
    /// parent, a hosted-root pointer, a directory entry. Such an address is
    /// exact (`tree-layout.md` S3).
    pub fn node(&self, slot: SlotId) -> Option<NodeRef<'a>> {
        let cell = self.page.cell(slot)?;
        Some(self.view(slot, cell))
    }

    /// The node `handle` names, or `None` if the handle is no longer good
    /// here: it is of another page or another lease, or a cell has left the
    /// page since it was issued.
    pub fn resolve(&self, handle: NodeHandle) -> Option<NodeRef<'a>> {
        let good = handle.lease() == self.lease
            && handle.page() == self.id()
            && handle.departures() == self.departures();
        if good {
            self.node(handle.slot())
        } else {
            None
        }
    }

    /// The nodes of the page, in slot order.
    pub fn nodes(&self) -> Nodes<'a> {
        Nodes {
            page: *self,
            cells: self.page.cells(),
        }
    }

    fn view(&self, slot: SlotId, cell: TreeCell<'a>) -> NodeRef<'a> {
        NodeRef {
            handle: NodeHandle::new(
                CellAddress::new(self.id(), slot),
                self.departures(),
                self.lease,
            ),
            cell,
        }
    }
}

/// The nodes of a page, in slot order.
#[derive(Debug, Clone)]
pub struct Nodes<'a> {
    page: NodePage<'a>,
    cells: Cells<'a>,
}

impl<'a> Iterator for Nodes<'a> {
    type Item = NodeRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (slot, cell) = self.cells.next()?;
        Some(self.page.view(slot, cell))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.cells.size_hint()
    }
}

impl DoubleEndedIterator for Nodes<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let (slot, cell) = self.cells.next_back()?;
        Some(self.page.view(slot, cell))
    }
}

impl FusedIterator for Nodes<'_> {}

/// One node, borrowed from the frame that holds its page.
///
/// Opening it read the cell's header and the fixed part of its links. Every
/// accessor returns bytes of the frame or a few numbers, and none allocates.
/// The value is not read unless [`Self::value`] returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRef<'a> {
    handle: NodeHandle,
    cell: TreeCell<'a>,
}

impl<'a> NodeRef<'a> {
    /// The node's name, to keep: good until a cell leaves the node's page or
    /// the lease ends.
    pub const fn handle(&self) -> NodeHandle {
        self.handle
    }

    /// Where the node's cell is.
    pub const fn address(&self) -> CellAddress {
        self.handle.address()
    }

    /// The node's key.
    pub fn key(&self) -> &'a [u8] {
        self.cell.key()
    }

    /// The node's value, the pointer to it in the value log, or what stands
    /// for a value that is not in the value log yet.
    pub fn value(&self) -> CellValue<'a> {
        self.cell.value()
    }

    /// The length of the node's value, wherever the value is.
    pub const fn value_len(&self) -> u32 {
        self.cell.value_len()
    }

    /// The node's feature type with its aggregate data, as its writer
    /// encoded it (`tree-page.md` TP12).
    pub fn feature(&self) -> &'a [u8] {
        self.cell.feature()
    }

    /// The node's value hash.
    pub fn value_hash(&self) -> &'a Hash {
        self.cell.value_hash()
    }

    /// The node's key-value hash.
    pub fn kv_hash(&self) -> &'a Hash {
        self.cell.kv_hash()
    }

    /// One of the hashes the node stores. `None` for the hash of a child the
    /// node does not have.
    pub fn hash(&self, field: HashField) -> Option<&'a Hash> {
        self.cell.hash(field)
    }

    /// Whether the node has a child on `side`.
    pub const fn has_link(&self, side: Side) -> bool {
        self.cell.has_link(side)
    }

    /// The link to the child on `side`: the address of the child's cell, the
    /// child's hash, its heights, the length of its key and its aggregate
    /// data.
    pub fn link(&self, side: Side) -> Option<TreeLink<'a>> {
        self.cell.link(side)
    }

    /// The roots of the Merks the node hosts, by position.
    pub fn hosted_roots(&self) -> HostedRoots<'a> {
        self.cell.hosted_roots()
    }

    /// Whether the node is the root of a fragment: no cell of its page
    /// points at it.
    pub const fn is_fragment_root(&self) -> bool {
        self.cell.is_fragment_root()
    }

    /// The node's cell: its own bytes, as they go to another page when the
    /// cell is carried.
    pub const fn cell(&self) -> TreeCell<'a> {
        self.cell
    }
}
