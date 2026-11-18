//! Changing the nodes of a tree page in a frame.

use lignin_format::{
    delta::Delta,
    tree_page::{CellContent, CellValue, Hash, HashField, Side, TreeCell, TreeLink, TreePageMut},
    CellAddress, PageId, PageSize, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::{
    error::NodeError,
    handle::{Lease, NodeHandle},
    view::{NodePage, NodeRef},
};

/// Who is told of every change that is made to a page.
///
/// A change is told once it is in the page, as the record that makes it
/// (`page-delta.md` PD1), and a change that the page refused is not told.
/// The changes of one page, in the order they are told, bring the page as it
/// was opened to the page as it is: they are what a run of that page holds.
/// A record that a page took is one a run takes.
pub trait ChangeSink {
    /// An empty page was started under the id `page`, with its count of
    /// departures at `departures`.
    fn created(&mut self, page: PageId, departures: u64);

    /// `change` was made to `page`.
    fn changed(&mut self, page: PageId, change: &Delta<'_>);
}

/// No one is told.
impl ChangeSink for () {
    fn created(&mut self, _: PageId, _: u64) {}

    fn changed(&mut self, _: PageId, _: &Delta<'_>) {}
}

impl<S: ChangeSink + ?Sized> ChangeSink for &mut S {
    fn created(&mut self, page: PageId, departures: u64) {
        (**self).created(page, departures);
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        (**self).changed(page, change);
    }
}

/// A tree page in a frame that its holder alone can change: the private copy
/// of a write transaction, under its exclusive latch.
///
/// Every change is one call of the page codec and one record
/// ([`ChangeSink`]). A change that is refused leaves the page as it was. A
/// hash, a child's address, a mark, and a link, feature or value that takes
/// the room of the one before it are written where they are and allocate
/// nothing. A change that gives a cell another length lays the cell out
/// again, which allocates in the codec.
///
/// A slot keeps its node through every change but [`NodeMut::remove`], which
/// is the one change that ends the handles into the page.
#[derive(Debug)]
pub struct NodePageMut<'a, S = ()> {
    page: TreePageMut<'a>,
    lease: Lease,
    sink: S,
}

impl<'a, S: ChangeSink> NodePageMut<'a, S> {
    /// Opens the tree page at the start of `frame`, a frame of pages of
    /// `size`. Every change is told to `sink`: `()` where no one is told.
    ///
    /// Handles are issued and followed under `lease`.
    pub fn open(
        frame: &'a mut [u8],
        size: PageSize,
        lease: Lease,
        sink: S,
    ) -> Result<Self, NodeError> {
        let page = TreePageMut::open(frame, size, FRAME_FORMAT)?;
        Ok(Self { page, lease, sink })
    }

    /// Starts an empty page with the id `page` in `frame`, a frame of pages
    /// of `size`, and tells `sink` of it and of every change after it.
    ///
    /// The page's count of departures starts at 0, whatever page had the id
    /// before: a handle into that page is of an earlier lease. So a page is
    /// started once under a lease, and a holder that starts a page again
    /// under an id it has read takes a new lease first (see [`Lease`]).
    pub fn create(
        frame: &'a mut [u8],
        size: PageSize,
        page: PageId,
        lease: Lease,
        mut sink: S,
    ) -> Result<Self, NodeError> {
        let created = TreePageMut::init(frame, size, page, 0)?;
        sink.created(page, 0);
        Ok(Self {
            page: created,
            lease,
            sink,
        })
    }

    /// The page as it is now, for reading.
    pub fn view(&self) -> NodePage<'_> {
        NodePage::of(self.page.page(), self.lease)
    }

    /// The page's id.
    pub fn id(&self) -> PageId {
        self.page.page().id()
    }

    /// The bytes the page still has for cells and their slots.
    pub const fn free_bytes(&self) -> usize {
        self.page.free_bytes()
    }

    /// Puts a new node into the page, in its lowest free slot
    /// (`tree-layout.md` S4), and returns its name.
    ///
    /// No handle into the page ends: a cell that arrives takes a slot that
    /// no handle names.
    pub fn insert(&mut self, node: &CellContent<'_>) -> Result<NodeHandle, NodeError> {
        let slot = self.page.page().lowest_free_slot();
        self.page.insert(slot, node)?;
        self.inserted(slot)
    }

    /// Puts a node into the page as its cell is, in the page's lowest free
    /// slot: a cell that is carried here from another page
    /// ([`NodeRef::cell`]).
    pub fn insert_cell(&mut self, cell: TreeCell<'_>) -> Result<NodeHandle, NodeError> {
        let slot = self.page.page().lowest_free_slot();
        self.page.insert_encoded(slot, cell)?;
        self.inserted(slot)
    }

    /// The node in `slot`, to change it. `None` if the slot is free.
    pub fn node_mut(&mut self, slot: SlotId) -> Option<NodeMut<'_, 'a, S>> {
        self.view().node(slot)?;
        Some(NodeMut { page: self, slot })
    }

    /// The node `handle` names, to change it. `None` if the handle is no
    /// longer good here ([`NodePage::resolve`]).
    pub fn resolve_mut(&mut self, handle: NodeHandle) -> Option<NodeMut<'_, 'a, S>> {
        self.view().resolve(handle)?;
        Some(NodeMut {
            page: self,
            slot: handle.slot(),
        })
    }

    fn inserted(&mut self, slot: SlotId) -> Result<NodeHandle, NodeError> {
        let page = NodePage::of(self.page.page(), self.lease);
        let node = page.node(slot).expect("the slot was just filled");
        self.sink.changed(
            page.id(),
            &Delta::Insert {
                slot,
                cell: node.cell(),
            },
        );
        Ok(node.handle())
    }

    fn change(&mut self, change: &Delta<'_>) -> Result<(), NodeError> {
        change.apply(&mut self.page)?;
        self.sink.changed(self.page.page().id(), change);
        Ok(())
    }
}

/// One node of a page that is open for changes.
///
/// It holds the page for as long as it lives, so the node stays in its slot.
#[derive(Debug)]
pub struct NodeMut<'p, 'a, S = ()> {
    page: &'p mut NodePageMut<'a, S>,
    slot: SlotId,
}

impl<S: ChangeSink> NodeMut<'_, '_, S> {
    /// The node as it is now.
    pub fn node(&self) -> NodeRef<'_> {
        self.page
            .view()
            .node(self.slot)
            .expect("the slot holds a cell while its page is held")
    }

    /// The node's name, to keep.
    pub fn handle(&self) -> NodeHandle {
        self.node().handle()
    }

    /// Replaces the node's value: by a value, by the pointer to one in the
    /// value log, or by a pending value. The node's hashes and links keep
    /// their bytes.
    pub fn set_value(&mut self, value: CellValue<'_>) -> Result<(), NodeError> {
        self.page.change(&Delta::Value {
            slot: self.slot,
            value,
        })
    }

    /// Sets, replaces or, with `None`, removes the link to the child on
    /// `side`.
    pub fn set_link(&mut self, side: Side, link: Option<TreeLink<'_>>) -> Result<(), NodeError> {
        self.page.change(&Delta::Link {
            slot: self.slot,
            side,
            link,
        })
    }

    /// Overwrites the address in the link to the child on `side`: the
    /// child's cell was carried, or another cell took its place.
    pub fn set_child(&mut self, side: Side, child: CellAddress) -> Result<(), NodeError> {
        self.page.change(&Delta::Child {
            slot: self.slot,
            side,
            child,
        })
    }

    /// Overwrites one of the hashes the node stores, where it is.
    pub fn set_hash(&mut self, field: HashField, hash: &Hash) -> Result<(), NodeError> {
        self.page.change(&Delta::Hash {
            slot: self.slot,
            field,
            hash,
        })
    }

    /// Replaces the node's feature type with its aggregate data.
    pub fn set_feature(&mut self, feature: &[u8]) -> Result<(), NodeError> {
        self.page.change(&Delta::Feature {
            slot: self.slot,
            feature,
        })
    }

    /// Marks the node as the root of a fragment, or takes the mark off.
    pub fn set_fragment_root(&mut self, fragment_root: bool) -> Result<(), NodeError> {
        self.page.change(&Delta::FragmentRoot {
            slot: self.slot,
            fragment_root,
        })
    }

    /// Overwrites the pointer to the root of the Merk the node hosts at
    /// position `index`.
    pub fn set_hosted_root(
        &mut self,
        index: u8,
        root: Option<CellAddress>,
    ) -> Result<(), NodeError> {
        self.page.change(&Delta::HostedRoot {
            slot: self.slot,
            index,
            root,
        })
    }

    /// Replaces all pointers to the roots of the Merks the node hosts, which
    /// may change their number.
    pub fn set_hosted_roots(&mut self, roots: &[Option<CellAddress>]) -> Result<(), NodeError> {
        self.page.change(&Delta::HostedRoots {
            slot: self.slot,
            roots: roots.into(),
        })
    }

    /// Replaces the node's cell by another cell of the same node: one with
    /// the same key. That it is the same node, and not a node of another
    /// Merk with an equal key, is the caller's to know (`tree-page.md` W1).
    pub fn replace(&mut self, cell: TreeCell<'_>) -> Result<(), NodeError> {
        self.page.change(&Delta::Replace {
            slot: self.slot,
            cell,
        })
    }

    /// Takes the node's cell out of the page: the node was deleted, or its
    /// cell was carried to another page.
    ///
    /// The page's count of departures goes up by one, so every handle into
    /// the page ends, those of the nodes that stay among them. A holder that
    /// still needs one takes it again from a view.
    pub fn remove(self) -> Result<(), NodeError> {
        self.page.change(&Delta::Remove { slot: self.slot })
    }
}
