//! The write path of one database: what a write transaction is made of.

use std::{
    error::Error,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use lignin_format::{CellAddress, CommitVersion, PageSize};
use lignin_store::{
    alloc::PageAllocator,
    cache::{BufferPool, OwnerId},
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};
use lignin_tree::{
    flat::{Committed, FlatTree, Private},
    fragment::Layout,
    node::Lease,
    value::InlinePolicy,
};

use super::{
    commits::Commits,
    key::{Column, Subtree},
    pages::CommittedPages,
    txn::WriteTxn,
};
use crate::snapshot::{OperationGuard, Versions};

/// The committed flat pages of one version, as one operation reads them:
/// they borrow the operation's guard, and so does every page they hand out
/// (`transactions.md` PN5).
pub type FlatReader<'s, 'g> = Committed<'s, 'g, OperationGuard<'s>>;

/// The flat pages of one write transaction in one of its operations:
/// private copies in the buffer pool, over the committed pages of the
/// version it reads under the operation's guard.
pub type FlatWriter<'s, 'g> = Private<'s, 'g, OperationGuard<'s>, &'s PageAllocator>;

/// What the flat pages of a transaction or of a cursor are bound to between
/// two operations, in place of a guard: nothing is read under it.
///
/// A reader or a writer is taken to its next operation with `under`, which
/// is sound at a version that stays pinned from one operation to the next
/// (`lignin_tree::flat`, "A writer's copies outlive its operation"). The
/// base pin of a transaction and the pin of a cursor are those pins.
#[derive(Debug)]
pub(crate) struct Idle;

impl lignin_tree::OperationGuard for Idle {}

/// The one [`Idle`].
pub(crate) static IDLE: Idle = Idle;

/// An error of the subtree directory.
pub type DirectoryError = Box<dyn Error + Send + Sync>;

/// What a write transaction asks of the subtree directory (TREE-05): where
/// the root cell of a Merk is.
///
/// An entry is no record (`transactions.md` ST3). It is derived from the
/// records, it is in no write set, and the transaction that changes the
/// root of a Merk rewrites the Merk's entry in the same operation (SC2):
/// when the root cell is carried, when another node becomes the root, and
/// when the Merk becomes empty or stops being empty (`tree-layout.md` S3).
/// The entries are in flat pages, so they are read and written through the
/// flat pages of the reader or of the transaction, and commit, roll back
/// and are made again with them.
pub trait Directory: Send + Sync {
    /// The address of the root cell of the Merk `subtree` in a committed
    /// version, or `None` for a Merk with no node, or one that is not
    /// there.
    fn committed_root(
        &self,
        pages: &FlatReader<'_, '_>,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, DirectoryError>;

    /// As [`Self::committed_root`], as a write transaction sees it.
    fn root(
        &self,
        pages: &FlatWriter<'_, '_>,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, DirectoryError>;

    /// The root cell of the Merk `subtree` is at `root` now, or the Merk
    /// has no node.
    fn set_root(
        &self,
        pages: &mut FlatWriter<'_, '_>,
        subtree: &Subtree,
        root: Option<CellAddress>,
    ) -> Result<(), DirectoryError>;

    /// The root cell that was at `from` is at `to` now: it was carried with
    /// its host, in a write that did not name its Merk
    /// (`tree-layout.md` L2, S3). An address that is the root of no Merk
    /// changes nothing.
    fn moved(
        &self,
        pages: &mut FlatWriter<'_, '_>,
        from: CellAddress,
        to: CellAddress,
    ) -> Result<(), DirectoryError>;
}

/// The flat keyspaces that hold the records which are not Merk nodes: one
/// tree for each column, in which a record of a subtree is stored under the
/// subtree's 32 bytes followed by its key (`flat-store.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keyspaces {
    /// The flat entries of the append-only trees.
    pub data: FlatTree,
    /// The aux records.
    pub aux: FlatTree,
    /// The base root key.
    pub roots: FlatTree,
    /// The meta records.
    pub meta: FlatTree,
}

impl Keyspaces {
    /// The tree of `column`.
    pub const fn of(&self, column: Column) -> FlatTree {
        match column {
            Column::Data => self.data,
            Column::Aux => self.aux,
            Column::Roots => self.roots,
            Column::Meta => self.meta,
        }
    }
}

/// Numbers the owners of private copies. No two owners of the process have
/// one number, whichever pool their copies are in.
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// The write path of one database: its versions, its pages, the key sets of
/// its commits, and where its records are kept.
///
/// It is made where the database is opened, of the structures of one store
/// (TXN-07), and every write transaction is begun from it.
#[derive(Clone, Copy)]
pub struct WritePath<'s> {
    pub(crate) versions: &'s Versions,
    pub(crate) loader: PageLoader<'s>,
    pub(crate) pool: &'s BufferPool,
    pub(crate) ids: &'s PageAllocator,
    pub(crate) values: &'s ValueStore,
    pub(crate) commits: &'s Commits,
    pub(crate) directory: &'s dyn Directory,
    pub(crate) keyspaces: Keyspaces,
    pub(crate) layout: Layout,
    pub(crate) inline: InlinePolicy,
}

/// The parts of a store that a [`WritePath`] is made of.
#[cfg_attr(
    any(not(test), loom),
    expect(dead_code, reason = "the engine's open (TXN-07) is its one caller")
)]
pub(crate) struct Store<'s> {
    pub(crate) versions: &'s Versions,
    pub(crate) loader: PageLoader<'s>,
    pub(crate) pool: &'s BufferPool,
    pub(crate) ids: &'s PageAllocator,
    pub(crate) values: &'s ValueStore,
    pub(crate) commits: &'s Commits,
    pub(crate) directory: &'s dyn Directory,
}

impl<'s> WritePath<'s> {
    /// The write path of the database whose structures `store` names, with
    /// its records in `keyspaces`, its Merk nodes laid out by `layout`, and
    /// the values of its nodes in their cells up to the threshold of
    /// `inline`. The caller vouches that all of them are of one store.
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "the engine's open (TXN-07) is its one caller")
    )]
    pub(crate) fn new(
        store: Store<'s>,
        keyspaces: Keyspaces,
        layout: Layout,
        inline: InlinePolicy,
    ) -> Self {
        Self {
            versions: store.versions,
            loader: store.loader,
            pool: store.pool,
            ids: store.ids,
            values: store.values,
            commits: store.commits,
            directory: store.directory,
            keyspaces,
            layout,
            inline,
        }
    }

    /// The versions of the database.
    pub fn versions(&self) -> &'s Versions {
        self.versions
    }

    /// The key sets of the database's commits.
    pub fn commits(&self) -> &'s Commits {
        self.commits
    }

    /// The flat keyspaces of the records.
    pub const fn keyspaces(&self) -> Keyspaces {
        self.keyspaces
    }

    /// The size of the store's pages.
    pub const fn page_size(&self) -> PageSize {
        self.layout.page_size()
    }

    /// Which values of Merk nodes are stored in their cells.
    pub const fn inline_policy(&self) -> InlinePolicy {
        self.inline
    }

    /// Begins a write transaction (`transactions.md` WT1). Any number of
    /// them may be open. It holds nothing until it writes: no version pin,
    /// no private copy and no page id (PN1).
    pub fn begin(&self) -> WriteTxn<'s> {
        WriteTxn::new(*self)
    }

    /// The committed flat pages at `version`, for the operation that holds
    /// `guard`. The version is pinned, or it is the latest version, read
    /// after the guard was taken.
    pub(crate) fn committed<'g>(
        &self,
        guard: &'g OperationGuard<'s>,
        version: CommitVersion,
    ) -> FlatReader<'s, 'g> {
        Committed::new(self.loader, self.values, guard, version)
    }

    /// The committed tree pages at `version`, read under `lease` in the
    /// operation that holds `guard`.
    pub(crate) fn committed_pages<'a>(
        &self,
        guard: &'a OperationGuard<'s>,
        version: CommitVersion,
        lease: Lease,
        buffers: &'a mut LoadBuffers,
    ) -> CommittedPages<'a, 's> {
        CommittedPages {
            loader: self.loader,
            values: self.values,
            guard,
            at: version,
            lease,
            buffers,
        }
    }

    /// An owner that no private copy has had.
    pub(crate) fn next_owner() -> OwnerId {
        OwnerId::new(NEXT_OWNER.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Debug for WritePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WritePath")
            .field("versions", &self.versions)
            .field("keyspaces", &self.keyspaces)
            .finish_non_exhaustive()
    }
}
