//! Page id allocation: which page ids are in use, and when the id of a freed
//! page may name another page.
//!
//! - [`PageAllocator`] hands out page ids, takes them back, and holds the
//!   id of a freed page until no reader can reach that page.
//! - [`Pins`] is what it asks of whoever keeps the version pins and the
//!   operation guards.
//! - [`Recovery`] makes the allocator of a database that is being opened,
//!   from the page table and the replayed log.
//!
//! `docs/lignindb/page-alloc.md` specifies the rules of this module, on top
//! of `docs/lignindb/transactions.md` (ID1 to ID3, PN6). The allocator
//! knows nothing of what a page holds, and it reads no page.
//!
//! ```
//! use std::sync::atomic::{AtomicU64, Ordering};
//!
//! use lignin_format::CommitVersion;
//! use lignin_store::alloc::{Epoch, PageAllocator, PageState, Pins};
//!
//! // Whoever keeps the pins and the guards answers for them. Here: one
//! // snapshot at most, and no operation that is still running.
//! struct OneSnapshot {
//!     latest: AtomicU64,
//!     snapshot: AtomicU64,
//!     epoch: AtomicU64,
//! }
//!
//! impl Pins for OneSnapshot {
//!     fn oldest_pin(&self) -> CommitVersion {
//!         let latest = self.latest.load(Ordering::SeqCst);
//!         let oldest = self.snapshot.load(Ordering::SeqCst).min(latest);
//!         CommitVersion::new(oldest).expect("a version")
//!     }
//!
//!     fn next_epoch(&self) -> Epoch {
//!         Epoch::new(self.epoch.fetch_add(1, Ordering::SeqCst) + 1)
//!     }
//!
//!     fn oldest_guard(&self) -> Epoch {
//!         Epoch::new(self.epoch.load(Ordering::SeqCst))
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let pins = OneSnapshot {
//!     latest: AtomicU64::new(0),
//!     snapshot: AtomicU64::new(u64::MAX),
//!     epoch: AtomicU64::new(0),
//! };
//! let ids = PageAllocator::new();
//! let mut released = Vec::new();
//!
//! // A transaction takes an id. Nobody else sees it until commit 1
//! // installs the page and is published.
//! let page = ids.allocate()?;
//! assert_eq!(ids.state(page), PageState::Private);
//! ids.publish(page)?;
//! pins.latest.store(1, Ordering::SeqCst);
//!
//! // A snapshot pins version 1, and then commit 2 frees the page.
//! pins.snapshot.store(1, Ordering::SeqCst);
//! ids.free(page, version(2))?;
//! pins.latest.store(2, Ordering::SeqCst);
//!
//! // The snapshot can still reach the page, so its id is not handed out.
//! ids.reclaim(&pins, &mut released);
//! assert_eq!(ids.state(page), PageState::Retired);
//! assert_ne!(ids.allocate()?, page);
//!
//! // Once the snapshot is dropped, the id is free, and is the next one out.
//! pins.snapshot.store(u64::MAX, Ordering::SeqCst);
//! ids.reclaim(&pins, &mut released);
//! assert_eq!(released, [page]);
//! assert_eq!(ids.allocate()?, page);
//! # Ok(())
//! # }
//! ```
//!
//! # The states of an id
//!
//! | State | Means | Left by |
//! | --- | --- | --- |
//! | [`Free`](PageState::Free) | Nothing uses the id. | [`allocate`](PageAllocator::allocate), which takes the lowest free id |
//! | [`Private`](PageState::Private) | An open transaction holds it, and nothing else has seen it. | [`publish`](PageAllocator::publish) when a commit installs the page, or [`release`](PageAllocator::release), after which it is free at once |
//! | [`Live`](PageState::Live) | It names a committed page. | [`free`](PageAllocator::free), with the version that freed the page |
//! | [`Retired`](PageState::Retired) | Its page was freed, and a reader may still reach it. | [`reclaim`](PageAllocator::reclaim), when no reader can |
//!
//! A call that does not fit the state of its id is refused with an
//! [`AllocError`] and changes nothing: an id cannot be freed twice, given
//! back after it was published, or freed before it was.
//!
//! # When the id of a freed page is used again
//!
//! A page that was freed at version *w* could be reached at versions below
//! *w*. Its id is handed out again only when both tests of
//! `transactions.md` PN6 are passed:
//!
//! 1. **No version pin can reach it** (ID2): the oldest pinned version is
//!    *w* or later. A snapshot, a cursor and a write transaction with
//!    writes each hold a pin.
//! 2. **No running operation can reach it** (ID3). An operation that read
//!    the latest version just before *w* was published holds no pin, and may
//!    have read the id. [`reclaim`](PageAllocator::reclaim) starts a new
//!    epoch once it has found test 1 passed, and frees the id when every
//!    operation guard of an earlier epoch has been released.
//!
//! The allocator takes both from [`Pins`], and it never waits: an id that
//! does not pass stays retired until a later call.
//!
//! # What the callers do
//!
//! - **A write transaction** keeps the ids it allocated. When it rolls
//!   back, or rolls back a scope or to a savepoint, it gives back the ids
//!   that the rolled-back part allocated. When it commits, it publishes the
//!   ids of the pages it installs and gives back the others. It gives an id
//!   back only when no rollback of its own can bring the page back.
//! - **A commit** frees a page with its own version, in the step that
//!   installs its page versions, before it is published.
//! - **Whoever frees a page** vouches for the version it passes: no handle
//!   that reads at that version or a later one reaches the page.
//! - **An operation** takes its guard before it reads the latest committed
//!   version, and holds it for as long as it uses a page id.
//!
//! # Other structures
//!
//! The allocator decides alone when an id is used again. It does not read
//! or change the page table. The chain of a freed page can still be in the
//! table when the id is handed out, and the table takes the base image of
//! the new page after the free link. A structure that is keyed by page id,
//! such as the buffer pool, tells the pages of one id apart by their
//! versions, as it must for a snapshot.
//!
//! # After a crash
//!
//! Nothing of the allocator is on disk. [`Recovery`] derives it from the
//! page table of the checkpoint and from the log groups that replay reads.
//! Ids that open transactions held are free again.

mod allocator;
mod error;
mod recovery;
mod states;

#[cfg(test)]
mod tests;

pub use allocator::{Census, Epoch, PageAllocator, PageState, Pins};
pub use error::AllocError;
pub use recovery::Recovery;
