//! The name of a node that is kept in memory, and the lease it is good
//! under.

use std::{
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use lignin_format::{CellAddress, PageId, SlotId};

static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);

/// One history of pages, as one holder reads it. A [`NodeHandle`] is good
/// only under the lease it was issued under.
///
/// Whoever opens views takes a lease and passes it to each of them. It may
/// keep using the lease for as long as both of these hold:
///
/// 1. **No page id it reads names two pages.** Every version it reads at is
///    one it has pinned, or a later one while that pin is held. The pin
///    holds back the reuse of every page id that can be seen from it on
///    (`transactions.md` ID2), and inside a write transaction a page id
///    names one page (ID1).
/// 2. **No frame it reads goes back.** What a frame holds is only changed
///    onward from what a handle was issued on: by the changes of
///    [`NodePageMut`](super::NodePageMut), by a commit, by a page step. A
///    frame that is put back to an earlier state, or replaced by one that
///    another history made, ends the lease: a rollback, a rollback to a
///    savepoint, a scope rollback, a rebase, the reconciliation of a scope
///    merge, a page that is started again under its id.
///
/// When either stops being true the holder takes a new lease, and every
/// handle of the old one is refused from then on. That is the rule of
/// `transactions.md` PN8 for a location kept between operations.
///
/// A lease is a number that no other lease of the process has. It is never
/// written to a file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lease(NonZeroU64);

impl Lease {
    /// A lease that no handle has been issued under.
    pub fn take() -> Self {
        let raw = NEXT_LEASE.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(raw).unwrap_or(NonZeroU64::MAX))
    }

    /// The lease's number.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lease#{}", self.0)
    }
}

impl fmt::Display for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lease {}", self.0)
    }
}

/// The name of a node, kept in memory: where its cell was, how many cells
/// had left its page by then, and the lease it was read under.
///
/// It is a hint (`tree-layout.md` S6). A view gives one for the node it
/// shows ([`NodeRef::handle`](super::NodeRef::handle)), and
/// [`NodePage::resolve`](super::NodePage::resolve) follows it only while it
/// still names that node:
///
/// - the page it is asked of is the page of the address
/// - the page's count of departures is the one the handle has: no cell has
///   left the page, so the slot holds the cell it held (`tree-page.md` TP7)
/// - the lease is the one the page is read under
///
/// A handle that is not followed says nothing about its node: the node is
/// found again by its key, from the root of its Merk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeHandle {
    address: CellAddress,
    departures: u64,
    lease: Lease,
}

impl NodeHandle {
    pub(super) const fn new(address: CellAddress, departures: u64, lease: Lease) -> Self {
        Self {
            address,
            departures,
            lease,
        }
    }

    /// Where the node's cell was when the handle was issued.
    pub const fn address(&self) -> CellAddress {
        self.address
    }

    /// The page of the address.
    pub const fn page(&self) -> PageId {
        self.address.page()
    }

    /// The slot of the address.
    pub const fn slot(&self) -> SlotId {
        self.address.slot()
    }

    /// How many cells had left the page when the handle was issued.
    pub const fn departures(&self) -> u64 {
        self.departures
    }

    /// The lease the handle was issued under.
    pub const fn lease(&self) -> Lease {
        self.lease
    }
}

impl fmt::Display for NodeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} after {} departures, under {}",
            self.address, self.departures, self.lease
        )
    }
}
