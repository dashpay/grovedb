//! The page table: one lock around the chains of every page.

use std::{
    fmt,
    sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use lignin_format::{CommitVersion, PageId};

use super::{chains::Chains, error::LinkError};
use crate::segment::BlockLocation;

/// What one link of a chain is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// The block holds the base image of the page as the link's version left
    /// it.
    Base(BlockLocation),
    /// The block holds a run of deltas that brings the page from the link
    /// before it to the link's version.
    Delta(BlockLocation),
    /// The page was freed at the link's version.
    Free,
}

/// One link of a page's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Link {
    /// The commit version the link brings the page to: the version at which
    /// the page version it completes was born.
    pub version: CommitVersion,
    /// What the link is.
    pub kind: LinkKind,
}

impl Link {
    /// A base image of the page as `version` left it, in the block at
    /// `location`.
    pub const fn base(version: CommitVersion, location: BlockLocation) -> Self {
        Self {
            version,
            kind: LinkKind::Base(location),
        }
    }

    /// A run of deltas that brings the page to `version`, in the block at
    /// `location`.
    pub const fn delta(version: CommitVersion, location: BlockLocation) -> Self {
        Self {
            version,
            kind: LinkKind::Delta(location),
        }
    }

    /// The mark of a page that the commit of `version` freed.
    pub const fn free(version: CommitVersion) -> Self {
        Self {
            version,
            kind: LinkKind::Free,
        }
    }

    /// The block the link points to, if it has one.
    pub const fn location(&self) -> Option<BlockLocation> {
        match self.kind {
            LinkKind::Base(location) | LinkKind::Delta(location) => Some(location),
            LinkKind::Free => None,
        }
    }
}

/// The page table: for every page id, the chain of blocks that hold the
/// page.
///
/// See [the module documentation](super) for what a chain is and for the
/// rules on changing one.
///
/// Every method takes `&self` and may be called from any thread. Each is one
/// step under the table's own lock: a read never sees half of a change, and
/// nothing else is locked or called while the lock is held.
pub struct PageTable {
    chains: RwLock<Chains>,
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PageTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chains = self.read_lock();
        f.debug_struct("PageTable")
            .field("pages", &chains.pages())
            .field("links", &chains.links())
            .field("high_water", &chains.high_water())
            .finish()
    }
}

impl PageTable {
    /// Bytes of memory one link takes.
    pub const LINK_BYTES: usize = Chains::LINK_BYTES;

    /// A table with no page.
    pub fn new() -> Self {
        Self::from_chains(Chains::new())
    }

    pub(super) fn from_chains(chains: Chains) -> Self {
        Self {
            chains: RwLock::new(chains),
        }
    }

    fn read_lock(&self) -> RwLockReadGuard<'_, Chains> {
        self.chains.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_lock(&self) -> RwLockWriteGuard<'_, Chains> {
        self.chains.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The newest link of `page`, or `None` if the page has no link.
    ///
    /// This is one indexed read: the page id is the index, and no chain is
    /// followed.
    pub fn newest(&self, page: PageId) -> Option<Link> {
        self.read_lock().newest(page)
    }

    /// Finds the blocks that hold `page` as a reader at the version `at`
    /// sees it, and returns the version at which that page version was born.
    ///
    /// `chain` is cleared and then holds the base image first and the delta
    /// runs to apply to it after it, in order. Returns `None`, with `chain`
    /// empty, if no link of the page is that old or the page was freed by
    /// then.
    ///
    /// The table knows what was written to segments. A change that only the
    /// log and the buffer pool hold has no link yet, so the answer is the
    /// newest page version at or before `at` that was written back.
    pub fn read(
        &self,
        page: PageId,
        at: CommitVersion,
        chain: &mut Vec<BlockLocation>,
    ) -> Option<CommitVersion> {
        self.read_lock().read(page, at, chain)
    }

    /// Adds every link of `page` to `out`, from the oldest to the newest.
    pub fn chain(&self, page: PageId, out: &mut Vec<Link>) {
        self.read_lock().chain(page, out);
    }

    /// Adds `link` to the chain of `page` as its newest link.
    ///
    /// Its version must not be lower than the version of the chain's newest
    /// link. A delta needs a base image or a delta beneath it, a freed page
    /// takes only a base image, and a location must be one a block can have.
    pub fn push(&self, page: PageId, link: Link) -> Result<(), LinkError> {
        self.write_lock().push(page, link)
    }

    /// Adds the base image at `base` to the chain of `page`, right after the
    /// link that points to `through`, and returns the version it has, which
    /// is that link's.
    ///
    /// The image must hold the page exactly as the chain up to and including
    /// that link gives it. Links that were added after `through` stay in
    /// front of the new image. The links the image replaces stay as well,
    /// for the readers that still need them, until [`Self::trim`] removes
    /// them.
    ///
    /// Fails with [`LinkError::NotInChain`] if no link points to `through`
    /// any more. The image is then of no use.
    pub fn consolidate(
        &self,
        page: PageId,
        through: BlockLocation,
        base: BlockLocation,
    ) -> Result<CommitVersion, LinkError> {
        self.write_lock().consolidate(page, through, base)
    }

    /// Points the link of `page` that points to `from` at `to` instead,
    /// where the same block has been copied.
    ///
    /// Fails with [`LinkError::NotInChain`] if no link points to `from` any
    /// more: the block was superseded and trimmed while it was copied, and
    /// the copy is dead.
    pub fn relocate(
        &self,
        page: PageId,
        from: BlockLocation,
        to: BlockLocation,
    ) -> Result<(), LinkError> {
        self.write_lock().relocate(page, from, to)
    }

    /// Removes from the chain of `page` every link that no reader at the
    /// version `oldest` or later needs, and adds the ones that point to a
    /// block to `removed`. Returns whether the page is left without a link.
    ///
    /// What stays is the newest base image at or before `oldest` and
    /// everything newer than it. If the page was freed at or before
    /// `oldest`, the mark and everything older go.
    pub fn trim(&self, page: PageId, oldest: CommitVersion, removed: &mut Vec<Link>) -> bool {
        self.write_lock().trim(page, oldest, removed).vacant
    }

    /// Removes every link with a version above `version`, from every chain,
    /// and returns how many there were.
    ///
    /// A recovery that lands on `version` calls this on the table it
    /// loaded. A checkpoint is named only once the log is durable through
    /// every link it holds ([`TableCheckpoints::install`]), so the answer
    /// is zero. If it is not, that rule was broken, and what is left is the
    /// table of `version` only if the links that the removed ones had
    /// replaced were still in their chains.
    ///
    /// [`TableCheckpoints::install`]: super::TableCheckpoints::install
    pub fn discard_after(&self, version: CommitVersion) -> u64 {
        self.write_lock().discard_after(version)
    }

    /// The number of pages that have at least one link. A page whose only
    /// link says it was freed counts.
    pub fn pages(&self) -> u64 {
        self.read_lock().pages()
    }

    /// The number of links in all chains.
    pub fn links(&self) -> u64 {
        self.read_lock().links()
    }

    /// The highest page id that has had a link since the table was made or
    /// loaded.
    pub fn high_water(&self) -> Option<PageId> {
        self.read_lock().high_water()
    }

    /// The bytes of memory the table holds.
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>() - std::mem::size_of::<Chains>()
            + self.read_lock().memory_usage()
    }

    /// Adds to `out` every link of the next pages that have one, from the
    /// page `from` on, and returns the page to go on from, or `None` once no
    /// page from `from` on has a link. The links are in ascending order of
    /// page id, and each chain is from its oldest link.
    ///
    /// One call is one step: it takes a run of at most 4096 page ids, so a
    /// walk over the whole table, from [`PageId::MIN`] until `None`, lets
    /// changes in between its calls. Each chain is given as it is at one
    /// moment. A page id that is not given had no link.
    pub fn scan(&self, from: PageId, out: &mut Vec<(PageId, Link)>) -> Option<PageId> {
        self.read_lock().scan(from, out)
    }
}
