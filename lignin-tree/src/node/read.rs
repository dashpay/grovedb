//! Reading nodes of committed pages through the store: the page from the
//! buffer pool, or loaded into it, and pinned while it is read.

use std::fmt;

use lignin_format::{flat_page::FlatPageKind, CellAddress, CommitVersion, PageId, SlotId};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
};

use super::{
    error::NodeError,
    handle::{Lease, NodeHandle},
    view::{NodePage, NodeRef},
};

/// What an operation holds while it reads (`transactions.md` PN5): for as
/// long as it lives, every frame the operation reached, every page-table
/// entry it read and every file location it took from one stays valid.
///
/// Whoever keeps the guards implements this for its guard (TXN-02). The
/// operation takes its guard before it reads the latest version, or before
/// it asks at a version it has pinned.
pub trait OperationGuard {}

/// The read path of one operation: the committed pages as a reader at one
/// version sees them.
///
/// It borrows the operation's guard, and what it hands out borrows it too:
/// a page or a node that was read here does not outlive the guard it was
/// read under.
///
/// A read takes a pin on a frame of the buffer pool and allocates nothing
/// itself. What the store allocates for it:
///
/// - nothing, when the page is in the pool
/// - nothing for a load, once `buffers` have grown to what the page's
///   blocks need, unless a record of the page's chain makes the page codec
///   lay a cell out again or gather the page's unused bytes
/// - a frame beside the pool's fixed ones, when the page is not in the pool
///   and no frame can be evicted because every one is pinned, private or
///   not yet written back. A reader that keeps pages pinned, as a cursor
///   keeps its parent stack, takes that into the size of its pool
pub struct NodeReader<'r, G: ?Sized> {
    loader: PageLoader<'r>,
    guard: &'r G,
    lease: Lease,
    at: CommitVersion,
}

impl<'r, G: OperationGuard + ?Sized> NodeReader<'r, G> {
    /// A reader of the pages of `loader`'s store at version `at`, for the
    /// operation that holds `guard`.
    ///
    /// `at` is a version the caller has pinned, or the latest version, read
    /// after `guard` was taken. Handles are issued and followed under
    /// `lease`.
    pub const fn new(
        loader: PageLoader<'r>,
        guard: &'r G,
        lease: Lease,
        at: CommitVersion,
    ) -> Self {
        Self {
            loader,
            guard,
            lease,
            at,
        }
    }

    /// The version the reader reads at.
    pub const fn at(&self) -> CommitVersion {
        self.at
    }

    /// The lease the reader issues and follows handles under.
    pub const fn lease(&self) -> Lease {
        self.lease
    }

    /// The guard of the reader's operation.
    pub const fn guard(&self) -> &'r G {
        self.guard
    }

    /// The tree page `page` as the reader's version has it, pinned: from the
    /// pool if it is there, and otherwise loaded.
    ///
    /// `buffers` is what a load reads into, kept by the caller from one
    /// read to the next.
    ///
    /// A frame that holds a tree page of another id than the one it is the
    /// frame of is refused ([`NodeError::WrongPage`]).
    pub fn pin(
        &self,
        page: PageId,
        buffers: &mut LoadBuffers,
    ) -> Result<PinnedPage<'r>, NodeError> {
        let frame = self.loader.get(page, self.at, buffers)?;
        PinnedPage::new(frame, self.lease)
    }

    /// The node at `address`, an address that a page stores: the link of a
    /// parent, a hosted-root pointer, a directory entry.
    ///
    /// Such an address is exact, so a slot that holds no cell is an error
    /// ([`NodeError::NoNode`]).
    pub fn node(
        &self,
        address: CellAddress,
        buffers: &mut LoadBuffers,
    ) -> Result<PinnedNode<'r>, NodeError> {
        let page = self.pin(address.page(), buffers)?;
        PinnedNode::new(page, address.slot()).ok_or(NodeError::NoNode { address })
    }

    /// The node `handle` names, or `None` if the handle is no longer good:
    /// it is of another lease, its page is no page or a flat page at the
    /// reader's version, or a cell has left the page since the handle was
    /// issued.
    ///
    /// The caller then finds the node by its key. A page that cannot be
    /// read, or a frame that holds neither a tree page nor a flat page, is
    /// an error and not a handle that went stale.
    pub fn resolve(
        &self,
        handle: NodeHandle,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, NodeError> {
        if handle.lease() != self.lease {
            return Ok(None);
        }
        let frame = match self.loader.get(handle.page(), self.at, buffers) {
            Ok(frame) => frame,
            Err(LoadError::NoPage { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let is_flat = frame
            .first()
            .is_some_and(|&kind| FlatPageKind::from_code(kind, FRAME_FORMAT).is_ok());
        if is_flat {
            return Ok(None);
        }
        let page = PinnedPage::new(frame, self.lease)?;
        let followed = page.view().resolve(handle).is_some();
        Ok(followed.then_some(PinnedNode {
            page,
            slot: handle.slot(),
        }))
    }
}

impl<G: ?Sized> Clone for NodeReader<'_, G> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<G: ?Sized> Copy for NodeReader<'_, G> {}

impl<G: ?Sized> fmt::Debug for NodeReader<'_, G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeReader")
            .field("at", &self.at)
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

/// One version of a tree page, pinned in the buffer pool.
///
/// Its bytes do not change and its frame is not reused while this lives.
/// Cloning takes another pin.
#[derive(Debug, Clone)]
pub struct PinnedPage<'r> {
    frame: PageRef<'r>,
    lease: Lease,
}

impl<'r> PinnedPage<'r> {
    fn new(frame: PageRef<'r>, lease: Lease) -> Result<Self, NodeError> {
        let found = NodePage::open(&frame, lease)?.id();
        let expected = frame.page();
        if found != expected {
            return Err(NodeError::WrongPage { found, expected });
        }
        Ok(Self { frame, lease })
    }

    /// The nodes of the page.
    pub fn view(&self) -> NodePage<'_> {
        NodePage::open(&self.frame, self.lease).expect("a tree page, checked when it was pinned")
    }

    /// The page's id.
    pub fn id(&self) -> PageId {
        self.frame.page()
    }

    /// The commit that bore this page version.
    pub fn born(&self) -> CommitVersion {
        self.frame.born()
    }

    /// The pinned frame.
    pub const fn frame(&self) -> &PageRef<'r> {
        &self.frame
    }
}

/// One node of a pinned page.
///
/// It keeps its page pinned, so the bytes that [`Self::node`] borrows do not
/// change while this lives.
#[derive(Debug, Clone)]
pub struct PinnedNode<'r> {
    page: PinnedPage<'r>,
    slot: SlotId,
}

impl<'r> PinnedNode<'r> {
    fn new(page: PinnedPage<'r>, slot: SlotId) -> Option<Self> {
        page.view().node(slot)?;
        Some(Self { page, slot })
    }

    /// The node.
    pub fn node(&self) -> NodeRef<'_> {
        self.page
            .view()
            .node(self.slot)
            .expect("the slot held a cell when the node was pinned")
    }

    /// The node's name, to keep.
    pub fn handle(&self) -> NodeHandle {
        self.node().handle()
    }

    /// The node's page.
    pub const fn page(&self) -> &PinnedPage<'r> {
        &self.page
    }

    /// The node's page, still pinned, without the node.
    pub fn into_page(self) -> PinnedPage<'r> {
        self.page
    }
}
