//! The tree pages of a write transaction: private copies in the buffer
//! pool, laid over the committed pages of the version it reads.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    tree_page::Side,
    CellAddress, CommitVersion, PageId, PageSize,
};
use lignin_store::{
    alloc::PageAllocator,
    cache::{BufferPool, CacheError, OwnerId, PrivateRef},
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};
use lignin_tree::{
    fragment::{FragmentError, PageMut, Pages},
    node::{
        ChangeSink, Lease, NodeError, NodeHandle, NodePage, NodePageMut, NodeReader, PinnedNode,
    },
};

use super::error::TxnError;
use crate::snapshot::OperationGuard;

/// The most cells a descent by key passes before the pages are taken for
/// damaged. A Merk of 2^64 nodes is at most 93 levels deep.
const MAX_DEPTH: usize = 128;

/// Tree pages that nodes are read from: a transaction's own copies over
/// committed pages, or committed pages alone.
pub(crate) trait NodePages {
    /// Reads the tree page `page`.
    fn page<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, TxnError>;
}

/// The committed tree pages of a store, as the operation that holds
/// `guard` reads them at one version.
pub(crate) struct CommittedPages<'a, 's> {
    pub(crate) loader: PageLoader<'s>,
    pub(crate) values: &'s ValueStore,
    pub(crate) guard: &'a OperationGuard<'s>,
    pub(crate) at: CommitVersion,
    pub(crate) lease: Lease,
    pub(crate) buffers: &'a mut LoadBuffers,
}

impl<'a> CommittedPages<'a, '_> {
    /// The node at `address`, pinned for the operation: what its value is
    /// read from, where that is not in its cell
    /// ([`read_value`](lignin_tree::value::read_value)).
    pub(crate) fn pinned(&mut self, address: CellAddress) -> Result<PinnedNode<'a>, TxnError> {
        let reader = NodeReader::new(self.loader, self.guard, self.lease, self.at);
        Ok(reader.node(address, self.buffers)?)
    }
}

impl NodePages for CommittedPages<'_, '_> {
    fn page<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, TxnError> {
        let frame = self.loader.get(page, self.at, self.buffers)?;
        Ok(read(NodePage::open(&frame, self.lease)?))
    }
}

/// Finds the node under `key` in the Merk whose root cell is at `root`, by
/// its key: down from the root, to the left where the key is smaller than
/// a node's and to the right where it is larger (`transactions.md` RD5).
///
/// This is the keyed lookup that a kept location is the shortcut for
/// (PN8). It reads one cell for each level.
pub(crate) fn find<P: NodePages>(
    pages: &mut P,
    root: Option<CellAddress>,
    key: &[u8],
) -> Result<Option<CellAddress>, TxnError> {
    let mut next = root;
    for _ in 0..MAX_DEPTH {
        let Some(address) = next else {
            return Ok(None);
        };
        let step = pages.page(address.page(), |page| {
            let node = page.node(address.slot())?;
            Some(match key.cmp(node.key()) {
                Ordering::Equal => None,
                Ordering::Less => Some(node.link(Side::Left).map(|link| link.child)),
                Ordering::Greater => Some(node.link(Side::Right).map(|link| link.child)),
            })
        })?;
        match step.ok_or(NodeError::NoNode { address })? {
            None => return Ok(Some(address)),
            Some(child) => next = child,
        }
    }
    Err(TxnError::Damaged("a Merk deeper than any tree of nodes"))
}

/// The address `handle` names, if the handle is of `lease` and its cell
/// has the key `key`.
pub(crate) fn follow<P: NodePages>(
    pages: &mut P,
    lease: Lease,
    handle: NodeHandle,
    key: &[u8],
) -> Option<CellAddress> {
    if handle.lease() != lease {
        return None;
    }
    let followed = pages.page(handle.page(), |page| {
        let node = page.resolve(handle)?;
        (node.key() == key).then(|| node.address())
    });
    followed.ok().flatten()
}

/// The sink of one private copy: every change the page took is put into
/// the run of that page, which the commit writes to the log
/// (`page-delta.md` PD1).
struct RunSink<'r> {
    run: &'r mut Vec<u8>,
}

impl ChangeSink for RunSink<'_> {
    fn created(&mut self, page: PageId, departures: u64) {
        self.run.clear();
        DeltaRunWriter::new(page, Some(departures), self.run);
    }

    fn changed(&mut self, _: PageId, change: &Delta<'_>) {
        change
            .encode_into(self.run)
            .expect("a record that a page took is one a run takes");
    }
}

/// One private copy of a tree page.
#[derive(Debug)]
pub(crate) struct TreeCopy<'s> {
    /// The copy, in the buffer pool.
    pub(crate) frame: PrivateRef<'s>,
    /// Whether the transaction made the page.
    pub(crate) created: bool,
    /// The run of the page: the records that take it from what it was to
    /// what it is, in the order they were made (`DeltaRun`). A copy that is
    /// as the page was has a run with no record.
    pub(crate) run: Vec<u8>,
}

impl TreeCopy<'_> {
    /// Whether the copy is as the page was: its run has no record.
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "the commit (TXN-05) is its one caller")
    )]
    pub(crate) fn is_unchanged(&self) -> bool {
        self.run.len() <= DeltaRun::HEADER_LEN
    }
}

/// The private tree pages of a write transaction, over the committed pages
/// of its base version (`transactions.md` WT3, WT4).
///
/// - A page is copied when a write first changes it, and from then on no
///   page step changes the page (PV7). A committed page that the
///   transaction frees keeps its copy for the same reason, until the commit
///   frees it.
/// - The id of a page the transaction made is the transaction's until its
///   commit publishes it, or until these pages are given up (ID1).
/// - Every change is told to the copy's run, which is what the commit logs.
pub(crate) struct TreePages<'s> {
    loader: PageLoader<'s>,
    pool: &'s BufferPool,
    ids: &'s PageAllocator,
    owner: OwnerId,
    at: CommitVersion,
    lease: Lease,
    size: PageSize,
    copies: BTreeMap<PageId, TreeCopy<'s>>,
    freed: BTreeMap<PageId, PrivateRef<'s>>,
    /// The ids of pages the transaction made and freed again, or could not
    /// start.
    unused: BTreeSet<PageId>,
    buffers: LoadBuffers,
}

impl<'s> TreePages<'s> {
    pub(crate) fn new(
        loader: PageLoader<'s>,
        pool: &'s BufferPool,
        ids: &'s PageAllocator,
        owner: OwnerId,
        at: CommitVersion,
        lease: Lease,
        size: PageSize,
    ) -> Self {
        Self {
            loader,
            pool,
            ids,
            owner,
            at,
            lease,
            size,
            copies: BTreeMap::new(),
            freed: BTreeMap::new(),
            unused: BTreeSet::new(),
            buffers: LoadBuffers::new(),
        }
    }

    /// The lease the pages are read and changed under.
    pub(crate) const fn lease(&self) -> Lease {
        self.lease
    }

    /// Reads and changes the pages under `lease` from here on: every
    /// handle that was issued under another has ended (PN8).
    pub(crate) fn set_lease(&mut self, lease: Lease) {
        self.lease = lease;
    }

    /// How many private copies the transaction holds: of pages it changed,
    /// made or freed.
    pub(crate) fn copies(&self) -> usize {
        self.copies.len() + self.freed.len()
    }

    fn check_live(&self, page: PageId) -> Result<(), TxnError> {
        if self.freed.contains_key(&page) || self.unused.contains(&page) {
            return Err(TxnError::PageFreed { page });
        }
        Ok(())
    }

    /// The transaction's copy of `page`, made now if it has none.
    fn copy(&mut self, page: PageId) -> Result<&mut TreeCopy<'s>, TxnError> {
        self.check_live(page)?;
        if !self.copies.contains_key(&page) {
            let owner = self.owner;
            // A copy that the pool has under this owner and that these
            // pages do not know was left by somebody else.
            if self.pool.private(owner, page).is_some() {
                return Err(CacheError::AlreadyPrivate { page, owner }.into());
            }
            // What was loaded is held while the copy is made: a page that
            // nothing pins can be evicted before the pool is asked again.
            let mut loaded = None;
            let frame = loop {
                match self.pool.make_private(owner, page, self.at) {
                    Ok(frame) => break frame,
                    Err(miss) => loaded = Some(self.loader.load(miss, &mut self.buffers)?),
                }
            };
            drop(loaded);
            let mut run = Vec::new();
            DeltaRunWriter::new(page, None, &mut run);
            let created = false;
            self.copies.insert(
                page,
                TreeCopy {
                    frame,
                    created,
                    run,
                },
            );
        }
        Ok(self.copies.get_mut(&page).expect("the copy was just made"))
    }

    /// Gives the pages up: every copy is discarded, and the ids of the
    /// pages the transaction made go back (ID1). This is a rollback, and
    /// what a rebase does before it makes the pages again (WT7).
    ///
    /// Every copy is discarded and every id given back, also if one of
    /// them fails: the first error is returned, what could not be given
    /// back stays, and a later call gives it back.
    pub(crate) fn discard(&mut self) -> Result<(), TxnError> {
        let mut result = Ok(());
        let mut keep = |error: TxnError| {
            if result.is_ok() {
                result = Err(error);
            }
        };
        let pool = self.pool;
        self.copies
            .retain(|page, copy| match pool.discard(&copy.frame) {
                Ok(()) => {
                    if copy.created {
                        self.unused.insert(*page);
                    }
                    false
                }
                Err(error) => {
                    keep(error.into());
                    true
                }
            });
        self.freed.retain(|_, copy| match pool.discard(copy) {
            Ok(()) => false,
            Err(error) => {
                keep(error.into());
                true
            }
        });
        let ids = self.ids;
        self.unused.retain(|page| match ids.release(*page) {
            Ok(()) => false,
            Err(error) => {
                keep(error.into());
                true
            }
        });
        result
    }

    /// Ends the pages and hands what they hold to the commit.
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "the commit (TXN-05) is its one caller")
    )]
    pub(crate) fn into_parts(mut self) -> TreeParts<'s> {
        TreeParts {
            written: std::mem::take(&mut self.copies).into_iter().collect(),
            freed: std::mem::take(&mut self.freed).into_iter().collect(),
            unused: std::mem::take(&mut self.unused).into_iter().collect(),
        }
    }
}

/// A transaction that is dropped gives its copies up.
impl Drop for TreePages<'_> {
    fn drop(&mut self) {
        // What the pool will not let go of now cannot be given up later
        // either: nothing is left that could.
        let _ = self.discard();
    }
}

/// What the tree pages of a transaction leave for its commit
/// (`transactions.md` CM3 step 7).
#[derive(Debug)]
#[cfg_attr(
    any(not(test), loom),
    expect(dead_code, reason = "the commit (TXN-05) is its one reader")
)]
pub(crate) struct TreeParts<'s> {
    /// The copies of the pages the transaction changed or made, in
    /// ascending order of page id. One that is as the page was is
    /// discarded, not installed.
    pub(crate) written: Vec<(PageId, TreeCopy<'s>)>,
    /// The copies of the committed pages the transaction freed, each kept
    /// until the commit frees its page.
    pub(crate) freed: Vec<(PageId, PrivateRef<'s>)>,
    /// The ids of the pages the transaction made and freed again.
    pub(crate) unused: Vec<PageId>,
}

impl NodePages for TreePages<'_> {
    fn page<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, TxnError> {
        self.check_live(page)?;
        match self.copies.get(&page) {
            Some(copy) => {
                let latch = copy.frame.read();
                Ok(read(NodePage::open(&latch, self.lease)?))
            }
            None => {
                let frame = self.loader.get(page, self.at, &mut self.buffers)?;
                Ok(read(NodePage::open(&frame, self.lease)?))
            }
        }
    }
}

impl Pages for TreePages<'_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.page(page, read).map_err(TxnError::into_pages)
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        let (size, lease) = (self.size, self.lease);
        let copy = self.copy(page).map_err(TxnError::into_pages)?;
        let mut latch = (copy.frame.write()).map_err(|error| TxnError::from(error).into_pages())?;
        let mut sink = RunSink { run: &mut copy.run };
        let sink: &mut dyn ChangeSink = &mut sink;
        let mut open = NodePageMut::open(&mut latch, size, lease, sink)?;
        Ok(change(&mut open)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        let pages = |error: TxnError| error.into_pages();
        let page = self.ids.allocate().map_err(|error| pages(error.into()))?;
        let frame = match self.pool.new_private(self.owner, page) {
            Ok(frame) => frame,
            Err(error) => {
                // The id goes back when the pages are given up.
                self.unused.insert(page);
                return Err(pages(error.into()));
            }
        };
        let (created, run) = (true, Vec::new());
        self.copies.insert(
            page,
            TreeCopy {
                frame,
                created,
                run,
            },
        );
        let copy = self.copies.get_mut(&page).expect("the copy was just made");
        let mut latch = (copy.frame.write()).map_err(|error| pages(error.into()))?;
        let mut sink = RunSink { run: &mut copy.run };
        let sink: &mut dyn ChangeSink = &mut sink;
        NodePageMut::create(&mut latch, self.size, page, self.lease, sink)?;
        Ok(page)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        let pages = |error: TxnError| error.into_pages();
        let pool = self.pool;
        let copy = self.copy(page).map_err(pages)?;
        if copy.created {
            // Nobody else has seen the page. Its id stays the
            // transaction's until no rollback can bring the page back.
            pool.discard(&copy.frame)
                .map_err(|error| pages(error.into()))?;
            self.copies.remove(&page);
            self.unused.insert(page);
        } else if let Some(copy) = self.copies.remove(&page) {
            self.freed.insert(page, copy.frame);
        }
        Ok(())
    }
}
