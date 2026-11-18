//! The handles a reader holds: a snapshot, and one operation of a reader.

use std::{fmt, sync::OnceLock};

use lignin_format::{CommitVersion, PageId};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, LoadError, PageLoader},
};
use lignin_tree::node::{Lease, NodeReader};

use super::{
    error::SnapshotError,
    versions::{OperationGuard, VersionPin, Versions},
};

impl Versions {
    /// A snapshot of the latest version, which reads the pages of `pages`'s
    /// store (RD1). A read with no transaction is one of these, held for
    /// one caller operation (RD2).
    pub fn snapshot<'s>(&'s self, pages: PageLoader<'s>) -> Snapshot<'s> {
        Snapshot {
            pin: self.pin(),
            pages,
            lease: OnceLock::new(),
        }
    }

    /// One operation that holds no version pin, as a write transaction
    /// with an empty write set makes them (PN1, RD4): it takes its guard,
    /// and then reads the latest version, at which it reads its pages.
    ///
    /// `lease` is the caller's, for the node handles it keeps from one
    /// operation to the next. With no pin it takes a new one whenever the
    /// latest version has moved (PN8, `lignin_tree::node`).
    pub fn operation<'s>(&'s self, pages: PageLoader<'s>, lease: Lease) -> Operation<'s> {
        let guard = self.guard();
        let at = guard.latest();
        Operation {
            guard,
            at,
            pages,
            lease: Leased::By(lease),
        }
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
            lease: Leased::BySnapshot(&self.lease),
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
#[derive(Clone, Copy)]
enum Leased<'o> {
    By(Lease),
    BySnapshot(&'o OnceLock<Lease>),
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
    pages: PageLoader<'o>,
    lease: Leased<'o>,
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
            Leased::By(lease) => lease,
            Leased::BySnapshot(lease) => *lease.get_or_init(Lease::take),
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
