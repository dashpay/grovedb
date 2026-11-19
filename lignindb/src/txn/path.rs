//! The write path of one database: what a write transaction is made of.

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use lignin_format::{CommitVersion, PageSize};
use lignin_store::{
    alloc::PageAllocator,
    cache::{BufferPool, OwnerId},
    load::{LoadBuffers, PageLoader},
    stats::Stats,
    values::ValueStore,
};
use lignin_tree::{
    directory::Directory,
    drop::{self, FlatDrops},
    flat::{Committed, FlatTree, Private},
    fragment::Layout,
    node::Lease,
    value::InlinePolicy,
};

use super::{commits::Commits, error::TxnError, key::Column, pages::CommittedPages, txn::WriteTxn};
use crate::snapshot::{Directories, OperationGuard, Versions};

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
    /// Every keyspace, as a drop and an enter are given them
    /// ([`Flats`](lignin_tree::drop::Flats)): one for each column, in the
    /// order of [`Column::ALL`]. A record of a subtree is under the
    /// subtree's 32 bytes and its key, with nothing before them.
    ///
    /// A column that is left out here keeps the records of a dropped
    /// subtree, and shows them to one that is entered again under its
    /// prefix.
    pub fn columns(&self) -> [drop::Column<'static>; Column::ALL.len()] {
        Column::ALL.map(|column| drop::Column {
            tree: self.of(column),
            lead: &[],
        })
    }

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
    /// The counts of the store, which are told of every write transaction
    /// that has a write (`lignin_store::stats`).
    pub(crate) stats: &'s Stats,
    /// The keyspace of the subtree directory (`lignin_tree::directory`).
    pub(crate) directory: FlatTree,
    /// The handles on the subtree directory that the engine keeps, one for
    /// each version that is read ([`Directories`]).
    pub(crate) directories: &'s Directories,
    /// The keyspace of the pages that drops unlinked and that wait to be
    /// freed (`lignin_tree::drop`).
    pub(crate) flat_drops: FlatDrops,
    pub(crate) keyspaces: Keyspaces,
    pub(crate) layout: Layout,
    pub(crate) inline: InlinePolicy,
}

/// The parts of a store that a [`WritePath`] is made of.
pub(crate) struct Store<'s> {
    pub(crate) versions: &'s Versions,
    pub(crate) loader: PageLoader<'s>,
    pub(crate) pool: &'s BufferPool,
    pub(crate) ids: &'s PageAllocator,
    pub(crate) values: &'s ValueStore,
    pub(crate) commits: &'s Commits,
    /// The counts of the store, made beside its write-back.
    pub(crate) stats: &'s Stats,
    /// The keyspace of the store's subtree directory: its root page, which
    /// is the same for good, and the configuration of its pages.
    pub(crate) directory: FlatTree,
    /// The handles on the subtree directory that are kept for the store's
    /// readers. A commit puts the handle of its version there before it
    /// publishes the version.
    pub(crate) directories: &'s Directories,
    /// The keyspace of the pages that the drops of the store unlinked and
    /// that wait to be freed: its root page, which is the same for good,
    /// and the configuration of its pages. Its records are entries of the
    /// flat pages of the transaction that drops, so they commit with it.
    pub(crate) flat_drops: FlatDrops,
}

impl<'s> WritePath<'s> {
    /// The write path of the database whose structures `store` names, with
    /// its records in `keyspaces`, its Merk nodes laid out by `layout`, and
    /// the values of its nodes in their cells up to the threshold of
    /// `inline`. The caller vouches that all of them are of one store.
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
            stats: store.stats,
            directory: store.directory,
            directories: store.directories,
            flat_drops: store.flat_drops,
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

    /// The counts of the store. A write transaction counts among its open
    /// ones while it has a write ([`Stats::open_writes`]).
    pub fn stats(&self) -> &'s Stats {
        self.stats
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

    /// The keyspace of the subtree directory.
    pub const fn directory(&self) -> FlatTree {
        self.directory
    }

    /// The keyspace of the pages that drops unlinked and that wait to be
    /// freed: what a reclaimer is given
    /// ([`Reclaimer::flat_drops`](lignin_tree::drop::Reclaimer::flat_drops)).
    pub const fn flat_drops(&self) -> FlatDrops {
        self.flat_drops
    }

    /// The subtree directory of the committed version `at`: a copy of the
    /// handle the engine keeps for it ([`Directories`]).
    ///
    /// No handle is loaded for a version that is read
    /// (`lignin_tree::directory`, "A handle is of the pages it was loaded
    /// from"): one that is loaded after a later commit landed and what a
    /// drop left was reclaimed takes a subtree that was below the dropped
    /// one for a subtree in use. `at` is a version the caller pins, or the
    /// latest version it read under the guard it holds, and for both a
    /// handle is kept.
    pub(crate) fn directory_at(&self, at: CommitVersion) -> Result<Directory, TxnError> {
        self.directories.at(at).ok_or(TxnError::Damaged(
            "no handle on the subtree directory is kept for a version that is read",
        ))
    }

    /// The handles on the subtree directory that the engine keeps.
    pub(crate) fn directories(&self) -> &'s Directories {
        self.directories
    }

    /// Begins a write transaction (`transactions.md` WT1). Any number of
    /// them may be open. It holds nothing until it writes: no version pin,
    /// no private copy and no page id (PN1). So it is with its first write
    /// that it counts as an open write transaction of the store.
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
            stats: self.stats,
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
