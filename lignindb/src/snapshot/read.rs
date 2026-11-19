//! The handles a reader holds: a snapshot, and one operation of a reader.

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use lignin_format::{CommitVersion, PageId};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, LoadError, PageLoader},
    values::ValueStore,
};
use lignin_tree::{
    flat::FlatTree,
    node::{Lease, NodeReader},
};

use super::{
    error::SnapshotError,
    records::{Handle, Loaded, Records},
    versions::{OperationGuard, VersionPin, Versions},
};
use crate::txn::Keyspaces;

/// The read path of one database: its versions, and its pages.
///
/// It is made in one place, where the database is opened, from the
/// [`Versions`] that the store's reclaimers ask and the loader of that same
/// store. Nothing outside this crate can make one, and every snapshot and
/// every operation is taken from it, so a pin is always on the versions of
/// the store whose pages it reads.
#[derive(Debug, Clone, Copy)]
pub struct ReadPath<'s> {
    versions: &'s Versions,
    pages: PageLoader<'s>,
    /// Where the records of the store are, for the readers that open
    /// subtrees ([`Operation::open`]).
    records: Option<Records<'s>>,
}

impl<'s> ReadPath<'s> {
    /// The read path of the database whose versions are `versions` and
    /// whose pages `pages` loads. The caller vouches that the two are of
    /// one store: a pin on other versions keeps nothing of these pages.
    #[cfg_attr(
        any(not(any(test, feature = "testing")), loom),
        expect(dead_code, reason = "the engine's open (TXN-07) is its one caller")
    )]
    pub(crate) fn new(versions: &'s Versions, pages: PageLoader<'s>) -> Self {
        Self {
            versions,
            pages,
            records: None,
        }
    }

    /// The same read path, with where the records of the store are: its
    /// value log, the flat keyspace of each column, and the keyspace of its
    /// subtree directory. Its readers open subtrees and read their records
    /// ([`Operation::open`]). The caller vouches that they are of the same
    /// store as the pages.
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "the engine's open (TXN-07) is its one caller")
    )]
    pub(crate) fn with_records(
        self,
        values: &'s ValueStore,
        keyspaces: Keyspaces,
        directory: FlatTree,
    ) -> Self {
        let records = Records {
            values,
            keyspaces,
            directory,
        };
        Self {
            records: Some(records),
            ..self
        }
    }

    /// The versions of the database.
    pub fn versions(&self) -> &'s Versions {
        self.versions
    }

    /// A snapshot of the latest version (RD1). A read with no transaction
    /// is one of these, held for one caller operation (RD2).
    ///
    /// Where the read path names the records of the store, the subtree
    /// directory of the snapshot's version is loaded here, and the version
    /// is chosen again if it was not the latest any more when the load
    /// ended: a handle is good for a reader's life only if it was loaded
    /// while the reader's version was the latest (see
    /// [the module](super#the-records-of-a-subtree)).
    pub fn snapshot(&self) -> Snapshot<'s> {
        let (pin, directory) = self.pin();
        Snapshot {
            pin,
            pages: self.pages,
            lease: OnceLock::new(),
            records: self.records,
            directory,
        }
    }

    /// A pin on the latest version, with the subtree directory of that
    /// version, loaded while it was the latest.
    fn pin(&self) -> (VersionPin<'s>, Option<Loaded>) {
        let Some(records) = &self.records else {
            return (self.versions.pin(), None);
        };
        loop {
            let pin = self.versions.pin();
            let guard = self.versions.guard();
            let loaded = records.load(self.pages, &guard, pin.version());
            if guard.latest() == pin.version() {
                return (pin, Some(loaded.map_err(Arc::new)));
            }
        }
    }

    /// One operation that holds no version pin, as a write transaction
    /// with an empty write set makes them (PN1, RD4): it takes its guard,
    /// and then reads the latest version, at which it reads its pages.
    ///
    /// `lease` is the caller's, kept from one operation to the next. It is
    /// given a new lease here whenever the latest version is not the one
    /// its last operation read, or that operation was on another database,
    /// so that a node handle that was read elsewhere is not followed (PN8).
    pub fn operation(&self, lease: &mut LatestLease) -> Operation<'s> {
        let guard = self.versions.guard();
        let at = guard.latest();
        Operation {
            guard,
            at,
            pages: self.pages,
            lease: Leased::Own(lease.at(self.versions.id(), at)),
            database: self.versions.id(),
            records: self.records,
            directory: Handle::Own(OnceLock::new()),
        }
    }
}

/// The lease of a reader that pins no version and reads the latest one
/// (`transactions.md` PN8): good for the node handles it read at one
/// version of one database, and replaced when it reads at another.
///
/// With no pin, a page id is good only until the next commit, which can
/// free the page and give its id to another. So the handles of such a
/// reader end when the latest version moves, and [`ReadPath::operation`]
/// sees to that. A reader that has other reasons to end them, as a write
/// transaction has when it rolls back, calls [`Self::renew`].
#[derive(Debug)]
pub struct LatestLease {
    lease: Lease,
    /// The database and the version its last operation read.
    read: Option<(u64, CommitVersion)>,
}

impl LatestLease {
    /// A lease that no handle has been issued under.
    pub fn new() -> Self {
        Self {
            lease: Lease::take(),
            read: None,
        }
    }

    /// The lease as it is now.
    pub fn lease(&self) -> Lease {
        self.lease
    }

    /// Ends every handle that was issued under the lease.
    pub fn renew(&mut self) {
        *self = Self::new();
    }

    /// The lease for an operation that reads `database` at `version`: the
    /// one of the operation before it if that read the same, and a new one
    /// otherwise.
    fn at(&mut self, database: u64, version: CommitVersion) -> Lease {
        let reads = (database, version);
        if self.read.is_some_and(|read| read != reads) {
            self.renew();
        }
        self.read = Some(reads);
        self.lease
    }
}

impl Default for LatestLease {
    fn default() -> Self {
        Self::new()
    }
}

/// A snapshot: one version of the database, pinned from the moment it was
/// taken until it is dropped (`transactions.md` RD1).
///
/// Every read through it, at any later time and from any thread, is of that
/// version. No commit that is published later is seen, and neither is
/// anything uncommitted. It takes no latch and waits for no writer (LT6).
///
/// It has one [`Lease`] for its life, under which the node handles read
/// through it are good (PN8): its version stays pinned for as long as the
/// lease is used, so a page id names one page for all of them.
pub struct Snapshot<'s> {
    pin: VersionPin<'s>,
    pages: PageLoader<'s>,
    lease: OnceLock<Lease>,
    records: Option<Records<'s>>,
    /// The subtree directory of the snapshot's version, loaded when the
    /// snapshot was taken, while that version was the latest. It is good
    /// for the snapshot's life.
    directory: Option<Loaded>,
}

impl<'s> Snapshot<'s> {
    /// The version the snapshot reads.
    pub fn version(&self) -> CommitVersion {
        self.pin.version()
    }

    /// The pin that keeps the version.
    pub fn pin(&self) -> &VersionPin<'s> {
        &self.pin
    }

    /// The snapshot's lease. It is taken when it is first asked for, so a
    /// snapshot that keeps no node handle takes none.
    pub fn lease(&self) -> Lease {
        *self.lease.get_or_init(Lease::take)
    }

    /// Begins one operation of the snapshot: it holds a guard of its own
    /// (PN5) and reads at the snapshot's version, under its lease.
    pub fn operation(&self) -> Operation<'_> {
        Operation {
            guard: self.pin.versions().guard(),
            at: self.version(),
            pages: self.pages,
            lease: Leased::Snapshot(&self.lease),
            database: self.pin.versions().id(),
            records: self.records,
            directory: Handle::Snapshot(self.directory.as_ref()),
        }
    }

    /// Refused: a snapshot is read-only (RD1). The entry points that write
    /// through a transaction answer the same for a snapshot.
    pub fn commit(&self) -> Result<(), SnapshotError> {
        Err(SnapshotError::ReadOnly)
    }

    /// Does nothing: a snapshot has no writes to give up (RD1).
    pub fn rollback(&self) {}
}

impl fmt::Debug for Snapshot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Snapshot").field(&self.version()).finish()
    }
}

/// Where an operation has its lease from.
enum Leased<'o> {
    Own(Lease),
    Snapshot(&'o OnceLock<Lease>),
}

/// One operation of a reader (`transactions.md` section 1): its guard, the
/// one version it reads at, and the pages of the store.
///
/// The guard was taken before the version was read or chosen. What the
/// operation reads borrows the operation, so no page, node, page id or
/// location it read is used after the guard is gone (PN5).
pub struct Operation<'o> {
    guard: OperationGuard<'o>,
    at: CommitVersion,
    pub(super) pages: PageLoader<'o>,
    lease: Leased<'o>,
    /// The database the operation reads ([`Versions::id`]).
    pub(super) database: u64,
    pub(super) records: Option<Records<'o>>,
    pub(super) directory: Handle<'o>,
}

impl<'o> Operation<'o> {
    /// The version the operation reads at: the version of its snapshot, or
    /// the latest version, read under its guard.
    pub fn at(&self) -> CommitVersion {
        self.at
    }

    /// The operation's guard.
    pub fn guard(&self) -> &OperationGuard<'o> {
        &self.guard
    }

    /// The lease the operation's node handles are issued and followed
    /// under: its snapshot's, or the one it was begun with.
    pub fn lease(&self) -> Lease {
        match self.lease {
            Leased::Own(lease) => lease,
            Leased::Snapshot(lease) => *lease.get_or_init(Lease::take),
        }
    }

    /// The version of `page` that the operation's version sees, pinned in
    /// its frame: from the buffer pool, or loaded into it.
    ///
    /// `buffers` is what a load reads into, kept by the caller from one
    /// read to the next.
    pub fn page(&self, page: PageId, buffers: &mut LoadBuffers) -> Result<PageRef<'_>, LoadError> {
        self.pages.get(page, self.at, buffers)
    }

    /// The nodes of the store as the operation's version has them, read
    /// under its guard and its lease.
    pub fn nodes(&self) -> NodeReader<'_, OperationGuard<'o>> {
        NodeReader::new(self.pages, &self.guard, self.lease(), self.at)
    }
}

impl fmt::Debug for Operation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Operation")
            .field("at", &self.at)
            .field("guard", &self.guard)
            .finish_non_exhaustive()
    }
}
