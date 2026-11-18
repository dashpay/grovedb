//! The name of a node that is kept in memory, and the lease it is good
//! under.

use std::{
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use lignin_format::{CellAddress, PageId, SlotId};

static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);

/// The next lease number of `counter`, or `None` once every number has been
/// given out. No number is given twice.
pub(super) fn issue(counter: &AtomicU64) -> Option<NonZeroU64> {
    let mut next = counter.load(Ordering::Relaxed);
    loop {
        let after = next.checked_add(1)?;
        match counter.compare_exchange_weak(next, after, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return NonZeroU64::new(next),
            Err(current) => next = current,
        }
    }
}

/// One line of descent of pages, as one holder reads it. A [`NodeHandle`] is
/// good only under the lease it was issued under.
///
/// A page's count of departures tells the states of that page apart along
/// one line only: a state, a state that was made from it by the changes of
/// [`NodePageMut`](super::NodePageMut), one made from that, and so on. Two
/// states that were made from one state side by side can have one count and
/// two cells in one slot, and so can a state and one that was put back and
/// changed again.
///
/// Whoever opens views takes a lease and passes it to each of them. It may
/// use the lease for as long as, for every page id, the states it reads
/// under the lease lie on one line: of any two, one was made from the
/// other. Three things keep that true, and when one of them stops being
/// true the holder takes a new lease, before it reads on:
///
/// 1. **No page id names two pages.** An id is given to another page only
///    once no pin can reach the page that had it (`transactions.md` ID2).
///    So the holder keeps pinned the oldest version it reads at under the
///    lease. A holder that has no pin (a write transaction that has written
///    nothing, PN1) reads at the latest version only, and takes a new lease
///    when the latest version it reads has moved. Inside a write
///    transaction a page id names one page (ID1). Which versions may be
///    read at all is the loader's rule, not this one: a version that is
///    pinned, or the latest one, read under the operation's guard.
/// 2. **No frame goes back.** A frame that is put back to an earlier state
///    ends the lease: a rollback, a rollback to a savepoint, a scope
///    rollback, a page that is started again under its id.
/// 3. **No two states go on side by side.** Two private copies of one page
///    are two lines from the moment both exist, so each write scope has a
///    lease of its own for what it reads and writes (LT4), and the
///    transaction takes a new one when it reconciles a merged scope. A
///    committed page of a later version than the one a private copy was
///    made from is beside that copy, not before it: a transaction takes a
///    new lease when it rebases, and reads such a page under another lease
///    until it has.
///
/// One thing the store keeps: a page step changes the current version of a
/// page and no older one, and none goes through while a private copy of the
/// page exists (`transactions.md` PV7). So the versions of a page are one
/// line, in whatever order they are read, and a private copy is on the line
/// of the version it was made from.
///
/// This is the rule of PN8 for a location that is kept between operations.
/// A handle of a lease that was given up is refused from then on.
///
/// A lease is a number that no other lease of the process has. It is never
/// written to a file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lease(NonZeroU64);

impl Lease {
    /// A lease that no handle has been issued under.
    ///
    /// # Panics
    ///
    /// When the process has taken 2^64 - 2 leases.
    pub fn take() -> Self {
        Self(issue(&NEXT_LEASE).expect("the process has lease numbers left"))
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
