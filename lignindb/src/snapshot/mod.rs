//! Snapshot reads: which version a reader reads, and what keeps it readable.
//!
//! - [`Versions`] is the latest published version of a database, the
//!   version pins that are held on it and on older ones, and the clock of
//!   retirements with the operation guards that are held against it.
//! - [`VersionPin`] keeps one version readable until it is dropped, and
//!   [`OperationGuard`] keeps what one operation read from being reclaimed
//!   under it.
//! - [`Snapshot`] is a version pin and the pages of the store, and
//!   [`Operation`] is one operation of a reader: its guard and the one
//!   version it reads at.
//! - [`Versions`] answers the three questions a reclaimer asks
//!   ([`Pins`](lignin_store::alloc::Pins)), and [`Reclaim`] is what it asks
//!   to reclaim when the oldest pin may have moved up.
//!
//! `docs/lignindb/transactions.md` has the rules: RD1 and RD2 for what a
//! snapshot reads, section 5 for pins and guards (PN1 to PN8), PV1 to PV3
//! for page versions, LT6 for readers and latches. `page-alloc.md` N1 to N4
//! says what a reclaimer is promised.
//!
//! ```
//! use lignin_format::CommitVersion;
//! use lignin_store::alloc::{Epoch, Pins};
//! use lignindb::snapshot::Versions;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let versions = Versions::new(CommitVersion::EMPTY);
//!
//! // Commit 1 is published, and a reader pins it.
//! versions.publish(version(1))?;
//! let pin = versions.pin();
//! assert_eq!(pin.version(), version(1));
//!
//! // Commits go on. The pin stays on its version, and holds back whatever
//! // was retired after it.
//! versions.publish(version(2))?;
//! versions.publish(version(3))?;
//! assert_eq!(pin.version(), version(1));
//! assert_eq!(versions.oldest_pin(), version(1));
//!
//! // An operation without a pin takes its guard, and then reads the latest
//! // version. A reclaimer that starts an epoch now waits for it.
//! let guard = versions.guard();
//! assert_eq!(guard.latest(), version(3));
//! let epoch = versions.next_epoch();
//! assert_eq!((guard.epoch(), epoch), (Epoch::new(0), Epoch::new(1)));
//! assert_eq!(versions.oldest_guard(), Epoch::new(0));
//!
//! // With both gone, nothing is held back.
//! drop((pin, guard));
//! assert_eq!(versions.oldest_pin(), version(3));
//! assert_eq!(versions.oldest_guard(), epoch);
//! # Ok(())
//! # }
//! ```
//!
//! # What a reader holds
//!
//! | Reader | Holds | Reads at |
//! | --- | --- | --- |
//! | a snapshot ([`Versions::snapshot`]) | a version pin for its life, and a guard for each operation ([`Snapshot::operation`]) | the pinned version |
//! | a read with no transaction (RD2) | a snapshot for one caller operation | the pinned version |
//! | an operation that pins nothing ([`Versions::operation`]): a write transaction with an empty write set | a guard | the latest version, read after the guard was taken |
//!
//! A cursor of a write transaction, a write transaction with writes and a
//! checkpoint being created hold a [`VersionPin`] as well (PN1); the tasks
//! that build them take it with [`Versions::pin`].
//!
//! # Taking a pin
//!
//! A pin is taken without a lock, and it cannot lose a race with
//! reclamation (PN4):
//!
//! 1. The reader reads the latest version and announces it in a cell of
//!    its own, where [`Pins::oldest_pin`](lignin_store::alloc::Pins::oldest_pin)
//!    looks.
//! 2. It reads the latest version again. If it is still the announced one,
//!    the pin is held. If a commit was published in between, the reader
//!    announces the newer version and looks again.
//!
//! `oldest_pin` reads the latest version first and the cells after it, and
//! answers with the smallest of what it saw. A pin whose announcement the
//! scan passed is in the answer. A pin that announced after the scan passed
//! its cell looked at the latest version later than `oldest_pin` did, so it
//! is on that version or a later one. Either way the answer is at most the
//! pin's version. A reader that was delayed between reading a version and
//! announcing it, and whose version was reclaimed meanwhile, finds a newer
//! latest version in step 2 and never uses the old one.
//!
//! A guard is taken by the same two steps against the epoch, so a guard is
//! of the epoch that was current when it was given out: every guard held
//! when [`Pins::next_epoch`](lignin_store::alloc::Pins::next_epoch) starts
//! an epoch is of an earlier one, and every guard given out afterwards is
//! of that epoch or a later one.
//!
//! All of it is `SeqCst` loads and stores of atomics: no lock, no wait, and
//! no unsafe code. A reader writes only to its own cell, which is on cache
//! lines of its own, so readers on different threads share no line that
//! any of them writes. A commit writes the latest version once, and a
//! reclaim the epoch once.
//!
//! # Reclaiming
//!
//! Nothing a reader could still reach is reused or deleted directly. It is
//! retired, and reclaimed when no pin needs it and every guard that was
//! held when that was found has been released (PN6). The page id allocator
//! ([`PageAllocator::reclaim`](lignin_store::alloc::PageAllocator::reclaim))
//! and the buffer pool
//! ([`BufferPool::reclaim`](lignin_store::cache::BufferPool::reclaim)) do
//! this by the three questions, and the cleaner will (STORE-08). They are
//! asked to when the oldest pin may have moved up (ID3, `page-alloc.md`
//! N8):
//!
//! - after a commit is published: the committer calls
//!   [`Versions::reclaim`] once it has given the commit lock back
//! - after a pin on a version before the latest is dropped: the pin calls
//!   it. A pin on the latest version holds nothing back, and its drop asks
//!   for nothing
//!
//! [`Versions::reclaim`] runs the [`Reclaim`] the versions were made with.
//! One runs at a time, and a call that finds one running leaves word for it
//! and returns, so a reader that drops a pin waits for no other thread.
//!
//! # What the callers do
//!
//! - **A committer** installs its page versions, then calls
//!   [`Versions::publish`] under the commit lock (CM3 steps 7 and 8), then
//!   asks for a reclaim. Nothing it installed is visible before the
//!   publish, because no reader asks at its version before then (PV1).
//! - **A reader of pages** asks only at a version it has pinned, or at the
//!   latest version it read under its guard, and holds the guard for the
//!   whole load (`lignin-store::load`). [`Operation`] is that in one value.
//! - **A reclaimer** asks the three questions in their order and never
//!   waits: what does not pass stays retired until a later call.
//! - **Whoever opens the database** makes the [`Versions`] with the version
//!   recovery landed on.
//!
//! # Models and tests
//!
//! - `tests/loom.rs`: the race of PN4, and the same for a guard, under
//!   loom: `RUSTFLAGS="--cfg loom" cargo test -p lignindb --release --lib snapshot::tests::loom`.
//! - `tests/model_check.rs`: [`Versions`] in the place of the model of pins
//!   and guards in the page id allocator's model check (`page-alloc.md`
//!   section 6), with the real allocator.
//! - `lignindb/tests/snapshot_reads.rs`: snapshots over a store on disk
//!   while a writer supersedes, frees and reuses pages.
//! - `lignindb/benches/snapshot_scaling.rs`: reader throughput by the
//!   number of reader threads, with one writer.

mod error;
mod read;
mod slots;
mod sync;
mod versions;

#[cfg(test)]
mod tests;

pub use error::{PublishError, SnapshotError};
pub use read::{Operation, Snapshot};
pub use versions::{OperationGuard, Reclaim, VersionPin, Versions};
