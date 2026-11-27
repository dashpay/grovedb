//! The buffer pool: the pages of a store, in memory.
//!
//! - [`BufferPool`] is a fixed number of frames, each the size of one page,
//!   and a table of what they hold.
//! - [`PageRef`] is one version of a page, pinned: its bytes, which do not
//!   change, for as long as it is held.
//! - [`Blank`] is a frame to fill, which nobody else can reach.
//! - [`PrivateRef`] is the private copy an open write transaction changes,
//!   with [`SharedLatch`] and [`ExclusiveLatch`] for one physical step on
//!   it.
//! - [`PageStep`] is one step of background work on a published page.
//!
//! The pool knows no tree, key or hash, and it reads and writes no file. A
//! page is bytes to it. `docs/lignindb/transactions.md` has the rules it
//! keeps: what a page version is (PV1 to PV7), private copies (WT4), latches
//! (LT2 to LT7), and what stays in memory because nothing else holds it
//! (FL6, FL7).
//!
//! ```
//! use lignin_format::{CommitVersion, PageId, PageSize};
//! use lignin_store::cache::{BufferPool, Loaded, OwnerId, PoolConfig};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let page = PageId::new(7).expect("not zero");
//! let pool = BufferPool::new(PoolConfig {
//!     page_size: PageSize::DEFAULT,
//!     capacity: 8 << 20,
//! })?;
//!
//! // A reader at version 3 asks for the page. It is not in the pool, so it
//! // is read from its segment into a frame. The page table says that
//! // commit 2 bore it and that nothing follows it.
//! let miss = pool.get(page, version(3)).expect_err("an empty pool");
//! let mut blank = pool.blank();
//! blank.fill(2);
//! let Loaded::Page(old) = pool.install_loaded(blank, miss, version(2), None)? else {
//!     unreachable!("nothing else changes the pool");
//! };
//! assert_eq!(old.born(), version(2));
//!
//! // A write transaction changes its own copy of the page.
//! let scope = OwnerId::new(1);
//! let copy = pool.make_private(scope, page, version(3)).expect("in the pool");
//! copy.write()?.fill(4);
//! assert_eq!(pool.get(page, version(3)).expect("committed")[0], 2);
//!
//! // Its commit, version 4, installs the copy. A reader of an older version
//! // keeps the page it had, and the pool keeps the new one until
//! // write-back has put it into a segment.
//! let new = pool.install(&copy, version(4))?;
//! assert_eq!(pool.get(page, version(3)).expect("still there")[0], 2);
//! assert_eq!(pool.get(page, version(4)).expect("installed")[0], 4);
//! assert_eq!(old[0], 2);
//! assert_eq!(pool.oldest_unwritten(), Some(version(4)));
//! pool.mark_written(&new)?;
//! # Ok(())
//! # }
//! ```
//!
//! # What a frame holds
//!
//! | A frame is | It holds | It leaves that state by |
//! | --- | --- | --- |
//! | free | nothing | [`BufferPool::blank`], and every call that needs a frame |
//! | blank | what its one holder writes | [`BufferPool::install_loaded`], [`PageStep::replace`], or the drop of the [`Blank`] |
//! | a page version | the content one commit gave one page id (PV1) | eviction, a page step that replaces it, or [`BufferPool::reclaim`] |
//! | a private copy | uncommitted changes of one owner (WT4) | [`BufferPool::install`], which makes it a page version, or [`BufferPool::discard`] |
//!
//! **A page version never changes** (PV2). So a reader takes no latch (LT6),
//! and the bytes behind a [`PageRef`] stay as they are whatever else happens
//! (LT2). Background work that has to change the bytes of a published page
//! does not edit them: it puts another frame in the page version's place
//! (PV4, [`PageStep::replace`]), and the old frame lives on for those who
//! hold it.
//!
//! **Page versions of one page id are told apart by their versions** (ID5).
//! A page version has the commit that bore it, and the version at which it
//! was superseded: by the next version of the page, or by the commit that
//! freed it. [`BufferPool::get`] returns the frame whose range holds the
//! version asked at. The id of a freed page can name a new page while frames
//! of the old one are still in the pool.
//!
//! A page version in the pool is in one of three states:
//!
//! | State | Means | Can be evicted |
//! | --- | --- | --- |
//! | unwritten | A commit installed it, or a page step changed it, and no segment holds that content. Write-back owes it. | no |
//! | held | No segment holds it, and none will: it is kept for the readers that can still ask for it (FL6, FL7). | no; [`BufferPool::reclaim`] drops it |
//! | written | A segment holds its content, and the page table has its link. | yes, once it has no pin |
//!
//! **A written page version that is superseded need not wait for the
//! clock.** Once the version after it is in a segment as well, or its page
//! is freed, nothing in the pool needs it: write-back checks a run against
//! the newest version of the page that a segment holds, and a reader of the
//! latest version reads the current one. It is *spent*: the pool lists it,
//! and [`BufferPool::reclaim`] takes a listed one out once no reader can
//! ask for it, as it drops a held one. Left to the clock it would keep its
//! frame until the hand had come by, twice if it was read since, and the
//! clock would take pages that are read to make room beside it: a commit
//! supersedes a version of every page it changes.
//!
//! The list is a help and no rule. A spent version that has a pin when its
//! turn comes, or that the list's bound dropped (4,096 are listed at most),
//! is left to the clock, like any written page version. And until the
//! version after it is written, a superseded written version is not spent:
//! it is the page that write-back applies the next run to before the run
//! is written. Nothing keeps it for that. It is an evictable frame, and if
//! the clock takes it first, write-back reads the page from the blocks of
//! its chain instead.
//!
//! # Reading
//!
//! [`BufferPool::get`] is the read path: one lookup in one part of the
//! pool's table, and one pin. A hit allocates nothing. So does a miss, and
//! so does a load into one of the fixed frames.
//!
//! **A hit on a page in a fixed frame takes no lock and writes nothing that
//! another reader of the page writes.** Every read of a tree starts at a
//! page that every other reader reads too, so a lock or a count that each
//! of them writes is what all of them would wait for.
//!
//! - The reader follows the frames of its part of the table without the
//!   lock. That part counts its changes, and the reader reads the count
//!   before it looks and after it has pinned: if the two differ, or a
//!   change is under way, it has pinned nothing, and it asks again under
//!   the lock.
//! - The pin is not a count on the frame. It is **announced**: the reader
//!   writes the frame's number into a cell of its thread's own lane, two
//!   cache lines that no other thread writes while no more threads run at
//!   one time than the pool has lanes. Whoever needs a frame to have no pin
//!   (eviction, a frame that a page step or [`BufferPool::reclaim`] took
//!   out) looks through the lanes as well as at the frame's count.
//! - The counts of hits and misses are kept in the lanes too.
//!
//! The shared lock of the part of the table, and a pin that is counted on
//! the frame, are what a reader falls back to: for a miss, for a page in a
//! frame beside the fixed ones, while that part of the table is being
//! changed, and when a thread holds more pins from hits at once than a lane
//! has cells (28). Every other pin is counted as well: a clone of a
//! [`PageRef`], and what a load, a commit, a page step and write-back are
//! given. A counted pin is as good as an announced one, and slower when
//! many threads take it on one frame.
//!
//! On a miss the caller (STORE-05) reads the page table **after** the miss,
//! fills a [`Blank`], and hands it over with the version of the link it
//! read and the version of the link that follows
//! ([`BufferPool::install_loaded`]). The pool then knows the versions the
//! frame stands for:
//!
//! - what follows a page version is either in a segment, and then the page
//!   table names it, or it is unwritten, and then it is in the pool, which
//!   does not evict it
//! - if a commit installed or freed a page, or a page step replaced a frame,
//!   in the same part of the table since the miss, the pool cannot tell
//!   that what was read is still right, and takes nothing
//!   ([`Loaded::Again`]): the caller asks again
//!
//! Both rest on one order, which is the caller's to keep: **the page table
//! is changed before the pool is told.** The free link of a page is pushed
//! before [`BufferPool::free`], and the link of a block before
//! [`BufferPool::mark_written`].
//!
//! Two loads of one page version can run at once. Both read the segment, the
//! first to finish installs its frame, and the second is given that frame.
//!
//! # Writing
//!
//! A write transaction changes private copies, and nobody else can reach
//! them (WT4, RD8). Each copy belongs to an [`OwnerId`]: a write scope, or
//! the transaction for what its scopes merged into it.
//!
//! - [`BufferPool::make_private`] copies the page version the owner reads.
//!   It makes one copy for an owner, whatever number of its operations ask
//!   at once (LT5), and gives each of them that copy.
//!   [`BufferPool::make_private_once`] gives the copy to the caller that
//!   makes it and refuses every other caller that asks this way: it is
//!   for an owner whose pages have one writer each, which then holds a
//!   copy only if it made it. [`BufferPool::new_private`] is for a page the transaction
//!   created, and refuses a second copy in the same way.
//! - The copy's bytes are behind its latch (LT3): [`PrivateRef::write`] for
//!   one physical step that changes them, [`PrivateRef::read`] to read them
//!   beside other operations of the owner. Two scopes never share a copy
//!   (LT4): each has its own, and [`BufferPool::transfer`] refuses to give
//!   an owner a second copy of a page.
//! - [`BufferPool::install`] is step 7 of a commit (CM3) for one page: the
//!   copy becomes the page version the commit bears, and the one before it
//!   is superseded. [`BufferPool::free`] is the same step for a page the
//!   commit frees. Neither is visible before the commit is published,
//!   because no reader asks at a version before it is.
//! - [`BufferPool::discard`] is rollback.
//!
//! **Latches are the caller's to order.** An operation that holds several
//! takes them in ascending order of page id and holds none when it returns
//! (LT3). [`PrivateRef::try_write`] and [`PrivateRef::try_read`] are for the
//! operation that finds it needs a lower id than one it holds.
//!
//! **The pool never waits for a latch.** It takes one in two places,
//! [`BufferPool::install`] and [`BufferPool::discard`], for the copy that
//! goes, before any other lock, and only if it is free: a copy whose latch
//! an operation still holds is refused ([`CacheError::Latched`]). A commit
//! runs when every scope of its transaction is closed (CM1), so it meets no
//! held latch, and it waits for none while it holds the counts lock (LT7).
//!
//! # Page steps
//!
//! Three things change the bytes of a published page without a commit: a
//! value pointer rewrite, the placing of a pending value, and the removal of
//! what a dropped subtree left (PV7). Each is one step with respect to every
//! commit that installs a version of the page and to the making of every
//! private copy of it. [`BufferPool::page_step`] takes the lock that makes
//! it so, and [`PageStep::replace`] makes the checks:
//!
//! 1. the frame that was looked at is still how the current version of the
//!    page is stored: no commit has superseded or freed the page, and no
//!    other step has replaced the frame
//! 2. no open write transaction holds a private copy of the page
//!
//! Whether the owning cell still holds what the caller expects (check 2 of
//! PV7) is the caller's to read from the frame. A frame's bytes do not
//! change, so what it read stays true for as long as check 1 holds.
//!
//! A page version is known again by [`PageRef::born`], and by nothing
//! finer: a frame that was evicted and loaded again is another frame of the
//! same page version. That is what the value log's cleaner keeps as its
//! `Found`.
//!
//! Making a private copy and installing one are page steps too. So a copy
//! is of the frame that stored its page version when the copy was made, no
//! step goes through while the copy exists, and a commit installs a copy
//! only over the page version it was made from. A step that went through is
//! therefore never undone by a commit (PV7).
//!
//! # Write-back
//!
//! The pool keeps, for each commit, the pages of which that commit bore a
//! version that no segment holds yet ([`BufferPool::unwritten_versions`],
//! [`BufferPool::unwritten_pages`]). Write-back (STORE-06) takes them oldest
//! first, pins each with [`BufferPool::get_version`], writes its block,
//! pushes its link, and calls [`BufferPool::mark_written`].
//! [`BufferPool::oldest_unwritten`] is the first version a table checkpoint
//! does not cover.
//!
//! - A page step can replace a frame while it is being written.
//!   [`BufferPool::mark_written`] then fails with [`CacheError::Replaced`],
//!   and the page is still on its list, for the new frame.
//! - A commit that frees a page takes what was owed of it off the lists
//!   (`page-table.md` T18): the page table refuses a link behind a free
//!   link, and write-back does not mark what it could not link. Those page
//!   versions are held instead, and dropped by [`BufferPool::reclaim`] once
//!   no reader can ask for them. Write-back can have such a frame pinned
//!   from a look it made before the page was freed:
//!   [`BufferPool::mark_written`] and [`BufferPool::hold_for_readers`] then
//!   fail with [`CacheError::NotUnwritten`], or with
//!   [`CacheError::Replaced`] once the frame was dropped, and write-back
//!   passes over it either way.
//! - A page version that was superseded with a pending value can be held
//!   as well, with [`BufferPool::hold_for_readers`], instead of being
//!   written (FL7).
//!
//! [`BufferPool::reclaim`] drops what is held once no reader can ask for it
//! (PN6). It asks [`Pins`](crate::alloc::Pins) the three questions the page
//! id allocator asks, and is called where that allocator's reclaim is.
//!
//! # Attached values
//!
//! A value that is too long for its cell and that no value-log file holds
//! yet lives with the frame of its page (WT4, FL7). Its cell in a tree
//! page, or its entry in a flat leaf, holds a pending value, and the pool
//! keeps the bytes beside the page under the eight bytes that name that
//! pending value ([`ValueId`]): what the page holds for the value is its
//! name here, and from its commit on that is its address in the log
//! (`log-format.md` LG21). The pool does
//! not look at the bytes, and compares the names only:
//! [`ExclusiveLatch::attach`] on a private copy, [`PageRef::attached`] to
//! read one. They are shared, not copied, when a page is copied.
//!
//! A page version with an attached value is not written: write-back places
//! each value in a page step whose new frame no longer has it, or holds the
//! page version for its readers.
//!
//! # Memory
//!
//! The pool asks for its memory when it is made: as many whole frames as
//! the configured size holds, their headers, the table and the lanes
//! ([`BufferPool::fixed_memory`]). What it counts is the bytes it asks for.
//! The allocator and the system may each take more for a request, which is
//! not counted: the system takes a mapping in whole pages of its own size.
//! On Linux and on Apple's systems no lock of the pool adds to it: none is
//! allocated when it is first taken.
//!
//! The headers, the table and the lanes are allocated and written then.
//! The frames are asked for zeroed, and the pool does not write them when
//! it is made. A pool with less than a megabyte of frames asks the
//! allocator for them. On Linux (x86-64 and AArch64) and on macOS, with
//! 64-bit pointers, a pool with a megabyte of frames or more asks the
//! system instead, for a mapping of its own
//! ([`BufferPool::mapped_memory`]), which the system gives zeroed. On any
//! other system, and where the system refuses a mapping, those frames are
//! the allocator's too. Either way the frames are counted in the fixed
//! memory from the start, used or not. What zeroed memory costs its giver,
//! and when, is not the pool's to say.
//!
//! Every frame starts at a multiple of 4,096 bytes.
//!
//! Beside its page a frame takes less than 512 bytes of it. A page that
//! is needed while every frame is in use gets the frame of a written page
//! version that has no pin, chosen by a clock that passes over a frame once
//! if it was used since the hand last came by. The clock has no lock:
//! each step of its hand is one eviction's, so the evictions of threads
//! that load at once wait for each other only at the lock of a part of
//! the table, where the frames they look at share one.
//!
//! When no frame can be evicted, because every one is pinned, private,
//! unwritten or held, the pool allocates a frame beside the fixed ones.
//! It does the same for an eviction that found none in as many steps of
//! its own as two rounds of the clock have, which, beside other evictions,
//! are not every frame. It
//! never refuses a page for lack of room. Such a frame is freed the moment
//! it could be evicted: when its last pin goes, if nothing else keeps it.
//! So **what the pool has allocated never exceeds the configured size plus
//! the frames that cannot be evicted**, with their attached values
//! ([`BufferPool::memory`], [`Census`]). The plan says "plus pinned
//! frames": a pin is one thing that keeps a frame, and a private copy and a
//! page version that no segment holds are the others. How many frames are
//! unwritten is write-back's to bound (FL5), and how many are private is
//! the transaction's (WT4). Of an attached value its bytes are counted, not
//! the list a frame keeps them in.
//!
//! A page in a frame beside the fixed ones is gone when it is unpinned. A
//! caller that loads a page in order to ask for it again, as before
//! [`BufferPool::make_private`], holds what it loaded until it has asked.
//!
//! # Locks
//!
//! In the order of LT7:
//!
//! | Level | Lock | Held for |
//! | --- | --- | --- |
//! | 4 | the latch of a private copy | one physical step of its owner |
//! | 5 | the page step lock of a page | one page step: a [`PageStep`], or inside the calls that install, free, copy, load, mark or drop a frame |
//! | below 5 | the pool's own: one part of its table, the free list, the unwritten lists, the list of frames that were taken out while they were pinned | a few instructions each, and for the last a look through the lanes. An eviction takes the lock of one part of the table for the frame it means to take, with no other lock of the pool held (the call that needs the frame may hold a page step lock, as a page step that takes a blank frame does), and a frame is put on the last list under the lock of its part of the table. Nothing else is taken under them, and no other module is called |
//!
//! No call into the pool waits for more than that. A reader that finds its
//! page in a fixed frame takes no lock at all, to find it or to give it
//! back, unless the frame was taken out of the table while the reader held
//! it: whoever gives back a pin of such a frame goes through the list of
//! those frames, and puts on the free list what has no pin left.
//! Otherwise a reader takes the shared lock of one part of the table to
//! find its page, and none to give it back, unless the page is in a frame
//! beside the fixed ones: the last pin of such a frame goes under the
//! exclusive lock of its part of the table, so that the frame leaves in
//! the same step.
//!
//! The page step lock is shared by the pages of one part of the table. So a
//! step on one page can make a step on another wait, never a reader, and a
//! thread holds one [`PageStep`] at a time.
//!
//! A thread that holds a [`PageStep`] calls nothing of the pool but that
//! guard's methods, [`BufferPool::get`], [`BufferPool::get_version`] and
//! [`BufferPool::blank`]. A caller that writes a record of its change
//! before it installs the change does so under the guard, after the checks:
//! that is the one thing the pool lets happen under level 5.
//!
//! # What the callers do
//!
//! - **The store's page size** is the manifest's, and a frame is that size.
//! - **A loader** reads the page table after the miss, and loads again when
//!   told to ([`Loaded::Again`]).
//! - **A commit** pushes a page's free link before it calls
//!   [`BufferPool::free`], installs and frees before it publishes its
//!   version, and has rebased its transaction, so that every copy it
//!   installs was made from the current version of its page.
//! - **Write-back** pushes a block's link before it calls
//!   [`BufferPool::mark_written`], and does not call it for a block whose
//!   link the page table refused.
//! - **Whoever keeps the pins** calls [`BufferPool::reclaim`] after a commit
//!   is published and after the oldest pin is dropped. A reader asks for a
//!   page only at a version it has pinned, or at the latest version under
//!   an operation guard (PN5).
//! - **Everyone** drops a [`PageRef`] when the view it serves ends. A pin
//!   costs memory, never correctness. A handle is given only to the pool it
//!   came from: another pool refuses it ([`CacheError::WrongPool`]).
//!
//! # Unsafe code
//!
//! The pool's unsafe code is in `cache/frame.rs` and nowhere else: the
//! allocation and the mapping of the frames, the walk of a reader that
//! holds no lock, and
//! the access to a frame's bytes through a pin, a latch or an owner. The
//! rules that make it sound are at the top of that file, and what they rest
//! on is kept inside it and in `cache/lanes.rs`, which has no unsafe code:
//! what a frame is (a private copy that can be written, or a page version
//! that cannot) changes only there, a frame of another pool is never linked
//! into this one, a part of the table is changed only between two steps of
//! its count of changes, and a pin that is announced is found by everyone
//! who looks for it afterwards. The rest of the pool is safe code over the
//! types that stand for the ways a frame may be reached.

mod error;
mod flush;
#[allow(unsafe_code)]
mod frame;
mod guard;
mod lanes;
mod pool;
mod sync;
mod write;

#[cfg(test)]
mod tests;

pub use error::{CacheError, StepRefusal};
pub use frame::ValueId;
pub use guard::{Blank, ExclusiveLatch, PageRef, PrivateRef, SharedLatch};
pub use pool::{BufferPool, Census, Loaded, Miss, PoolConfig, PoolStats};
pub use write::{NotMade, OwnerId, PageStep};
