//! The handles a reader holds: a snapshot, and one operation of a reader.

use std::fmt;

use lignin_format::{CommitVersion, PageId};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, LoadError, PageLoader},
};

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
        }
    }

    /// One operation that holds no version pin, as a write transaction
    /// with an empty write set makes them (PN1, RD4): it takes its guard,
    /// and then reads the latest version, at which it reads its pages.
    pub fn operation<'s>(&'s self, pages: PageLoader<'s>) -> Operation<'s> {
        let guard = self.guard();
        let at = guard.latest();
        Operation { guard, at, pages }
    }
}

/// A snapshot: one version of the database, pinned from the moment it was
/// taken until it is dropped (`transactions.md` RD1).
///
/// Every read through it, at any later time and from any thread, is of that
/// version. No commit that is published later is seen, and neither is
/// anything uncommitted. It takes no latch and waits for no writer (LT6).
pub struct Snapshot<'s> {
    pin: VersionPin<'s>,
    pages: PageLoader<'s>,
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

    /// Begins one operation of the snapshot: it holds a guard of its own
    /// (PN5) and reads at the snapshot's version.
    pub fn operation(&self) -> Operation<'_> {
        Operation {
            guard: self.pin.versions().guard(),
            at: self.version(),
            pages: self.pages,
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

/// One operation of a reader (`transactions.md` section 1): its guard, the
/// one version it reads at, and the pages of the store.
///
/// The guard was taken before the version was read or chosen. What the
/// operation reads borrows the operation, so no page, page id or location
/// it read is used after the guard is gone (PN5).
pub struct Operation<'o> {
    guard: OperationGuard<'o>,
    at: CommitVersion,
    pages: PageLoader<'o>,
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

    /// The read path of the store, for a reader of pages that is given the
    /// guard and the version beside it.
    pub fn loader(&self) -> PageLoader<'o> {
        self.pages
    }

    /// The version of `page` that the operation's version sees, pinned in
    /// its frame: from the buffer pool, or loaded into it.
    ///
    /// `buffers` is what a load reads into, kept by the caller from one
    /// read to the next.
    pub fn page(&self, page: PageId, buffers: &mut LoadBuffers) -> Result<PageRef<'_>, LoadError> {
        self.pages.get(page, self.at, buffers)
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
