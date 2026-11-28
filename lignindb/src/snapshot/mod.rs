//! Snapshot reads: which version a reader reads, and what keeps it readable.
//!
//! - [`Versions`] is the latest published version of a database, the
//!   version pins that are held on it and on older ones, and the clock of
//!   retirements with the operation guards that are held against it.
//! - [`VersionPin`] keeps one version readable until it is dropped, and
//!   [`OperationGuard`] keeps what one operation read from being reclaimed
//!   under it.
//! - [`ReadPath`] binds the versions of a database to its pages.
//!   [`Snapshot`] is a version pin and those pages, and [`Operation`] is
//!   one operation of a reader: its guard and the one version it reads at.
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
//! | a snapshot ([`ReadPath::snapshot`]) | a version pin for its life, and a guard for each operation ([`Snapshot::operation`]) | the pinned version |
//! | a read with no transaction (RD2) | a snapshot for one caller operation | the pinned version |
//! | an operation that pins nothing ([`ReadPath::operation`]): a write transaction with an empty write set | a guard, and a [`LatestLease`] from one operation to the next | the latest version, read after the guard was taken |
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
//! no unsafe code. The one exception is a table of cells that is full. It
//! grows by a chunk, which is linked through a `OnceLock`: a thread that
//! links one can wait there for another that is linking the same chunk,
//! for as long as a pointer takes to store. Safe code has no way to
//! publish a pointer that never waits, and this crate has no unsafe code.
//! The table starts with 256 cells for pins and as many for guards, doubles
//! and never shrinks, so that is met a few times in the life of a process,
//! and only while more pins or guards are held at once than ever before. A
//! count of the chunks, which is `SeqCst`, orders a claim in a new chunk
//! before the scans that have to see it.
//!
//! A reader writes to its own cell, which is on cache lines of its own, so
//! taking and dropping pins and guards on different threads writes no line
//! that two of them share. A commit writes the latest version once, and a
//! reclaim writes the epoch once and reads every cell.
//!
//! An answer stays true, and a later answer can still be lower than an
//! earlier one: a reader that has announced a version and has not looked
//! again yet is in the answer with it. A reclaimer uses each answer as the
//! bound it is, and does not take the answers to rise.
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
//!   [`Versions::reclaim`] once it has given the commit lock back, or has
//!   background work call it
//! - after a pin on a version before the latest is dropped. The reader
//!   does not reclaim: it tells the [`Reclaim`] that one is due
//!   ([`Reclaim::wake`]), and background work calls [`Versions::reclaim`].
//!   A reader does the same when it announced a version, found a newer one
//!   published and announced that instead: the first announcement may have
//!   held back the reclaim after that commit. A pin on the latest version
//!   holds nothing back, and its drop tells nobody
//!
//! [`Versions::reclaim`] runs the [`Reclaim`] the versions were made with.
//! One runs at a time, and a call that finds one running leaves word for it
//! and returns. A thread runs at most two in one call, so none is kept
//! reclaiming for the others. With a [`Reclaim`] that does not act on
//! [`Reclaim::wake`], what a reader held back is taken by the reclaim after
//! the next commit: that costs space for as long, never correctness.
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
//!   recovery landed on, and one [`ReadPath`] from them and the loader of
//!   the same store. That is the one place where the two are paired, and
//!   it is inside this crate. Its [`Reclaim`] runs the store's reclaimers, and its
//!   [`Reclaim::wake`] lets background work know.
//!
//! # The records of a subtree
//!
//! A reader finds the records of a subtree through the subtree directory
//! of the version it reads ([`Operation::open`], `tree-layout.md` D6): in
//! every column, the records under a prefix are those of the entry that
//! the directory has under it, and where it has no entry in use there is
//! no record to read. So a subtree that was dropped, and every subtree
//! that was below it, is gone for every reader from the drop's version on,
//! in every column, while what it left waits in the pages to be reclaimed;
//! and a snapshot from before the drop reads all of it for as long as it
//! is held (`transactions.md` CV9, T-CL1).
//!
//! [`Opened`] is a subtree that was opened, and the flat records of the
//! subtree are read through it alone: by key, and under a cursor
//! ([`RecordCursor`]). The root of its Merk is in its entry, and the nodes
//! are read from there ([`Operation::nodes`]).
//!
//! A handle on the directory is of the pages it was loaded from, and
//! **a handle is good for a version only if it was made while that version
//! was the newest.** What a drop left is taken out of the pages one step
//! at a time, the record of the dropped subtree last, and each step is on
//! the page versions that are current then. A reader of an older version
//! can still have the entry of a subtree that was below the dropped one in
//! a page version of its own, while the record of the drop is gone from a
//! page it shares with the latest version: a handle that is loaded then
//! does not know of the drop, and would open that subtree.
//!
//! So **no reader loads a handle. The engine keeps one for each version**
//! ([`Directories`]): the handle the frames of the commit had, put there
//! before the version is published, or the one the open of the database
//! loaded. A reader takes a copy, which shares the ids:
//!
//! - **A snapshot** takes it when it first opens a subtree, and keeps it
//!   for its life. [`ReadPath::snapshot`] reads no page.
//! - **An operation that pins no version** takes the handle of the latest
//!   version it read under its guard, when it first opens a subtree.
//!
//! A kept handle goes when no reader can ask for it any more, by the rule
//! that frees a page id: no pin is on its version or an older one, and
//! every guard that was held when that was found has been released. The
//! database's [`Reclaim`] sees to it ([`Directories::reclaim`]).
//!
//! # Models and tests
//!
//! - `tests/loom.rs`: the race of PN4, the same for a guard, a pin that
//!   makes the table grow, and two calls for a reclaim, under loom:
//!   `RUSTFLAGS="--cfg loom" cargo test -p lignindb --release --lib snapshot::tests::loom`.
//! - `tests/model_check.rs`: [`Versions`] in the place of the model of pins
//!   and guards in the page id allocator's model check (`page-alloc.md`
//!   section 6), with the real allocator.
//! - `tests/reads.rs`, over the store of `tests/store.rs`: snapshots over a
//!   store on disk while a writer supersedes, frees and reuses pages.
//! - `tests/scaling.rs`: reader throughput by the number of reader threads,
//!   with one writer. It is left out of a normal run:
//!   `cargo test --release -p lignindb --lib -- --ignored --nocapture reader_throughput`.

mod error;
mod kept;
mod nodes;
mod read;
mod records;
mod slots;
mod sync;
mod versions;

#[cfg(test)]
mod tests;

pub use error::{PublishError, SnapshotError};
pub use kept::Directories;
#[cfg(test)]
pub(crate) use nodes::held;
pub use nodes::NodeWalk;
pub use read::{LatestLease, Operation, ReadPath, Snapshot};
pub use records::{EntryWalk, Kept, Listed, Opened, ReadError, RecordCursor, RecordWalk};
pub use versions::{OperationGuard, Reclaim, VersionPin, Versions};
