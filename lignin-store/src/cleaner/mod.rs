//! The cleaner: dead space in segments and in the value log is found and
//! given back (STORE-08).
//!
//! Segments are written once and never changed. A page that is written
//! back again leaves its older blocks behind, and so does a page that is
//! freed. The cleaner is what makes that space go:
//!
//! - It keeps the **accounting**: for each segment, the bytes of the blocks
//!   that a link of the [page table](crate::table) points to, and the bytes
//!   of the others ([`Space`]).
//! - It **trims** the chains that hold links no reader needs any more.
//! - It **moves** the live blocks out of the segments that are mostly dead,
//!   into a segment of its own, and points their links at the copies.
//! - It **deletes** a segment once nothing points into it and nobody can
//!   still be reading it.
//! - It drives the cleaning of the [value log](crate::values), whose rules
//!   are that module's.
//!
//! `docs/lignindb/transactions.md` has the rules it keeps (CL1 to CL7,
//! PN6, CK3) and `page-table.md` those of the table (T10, T11, K6). It
//! knows no tree, key or hash, and it never looks into a block.
//!
//! [`CleanerState`] is what the cleaner keeps for one open store, made
//! beside the state of [write-back](crate::flush). [`Cleaner`] is a view
//! of write-back's stores and of that state. The store has no thread of
//! its own: whoever opens it runs [`Cleaner::run`] on one.
//!
//! ```
//! use std::sync::{
//!     atomic::{AtomicU64, Ordering},
//!     Arc, Mutex,
//! };
//!
//! use lignin_format::{
//!     manifest::Manifest,
//!     segment::{BlockContent, Compression},
//!     CommitVersion, PageId, PageSize,
//! };
//! use lignin_io::StdVfs;
//! use lignin_store::{
//!     alloc::{Epoch, Pins},
//!     cache::{BufferPool, PoolConfig},
//!     cleaner::{Cleaner, CleanerConfig, CleanerState},
//!     flush::{FlushConfig, FlushState, Published, Stores, WriteBack},
//!     segment::{ManifestStore, SegmentStore},
//!     table::{Link, PageTable, TableCheckpoints},
//!     values::ValueStore,
//!     wal::{LogOptions, LogReader, LogStore},
//! };
//!
//! // Who publishes commits and keeps the pins is the transaction layer.
//! // Here nothing is pinned, and no operation outlives a call.
//! struct Versions(AtomicU64);
//!
//! impl Pins for Versions {
//!     fn oldest_pin(&self) -> CommitVersion {
//!         self.latest()
//!     }
//!
//!     fn next_epoch(&self) -> Epoch {
//!         Epoch::new(1)
//!     }
//!
//!     fn oldest_guard(&self) -> Epoch {
//!         Epoch::new(1)
//!     }
//! }
//!
//! impl Published for Versions {
//!     fn latest(&self) -> CommitVersion {
//!         CommitVersion::new(self.0.load(Ordering::SeqCst)).expect("a version")
//!     }
//!
//!     fn guarded(&self, work: &mut dyn FnMut()) {
//!         work()
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // A new store, opened, with write-back's view of it.
//! let dir = tempfile::tempdir()?;
//! let size = PageSize::DEFAULT;
//! let vfs = Arc::new(StdVfs);
//! let empty = Manifest::new(size, Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs.clone(), dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//! let values = ValueStore::open(&manifest, &[])?;
//! let checkpoints = TableCheckpoints::open(&manifest)?;
//! let end = LogReader::new(vfs, dir.path(), manifest.manifest()?).into_end()?;
//! let log = LogStore::open(&manifest, &end, LogOptions::default())?;
//! log.roll(&mut manifest)?;
//! let table = PageTable::new();
//! let pool = BufferPool::new(PoolConfig {
//!     page_size: size,
//!     capacity: 8 << 20,
//! })?;
//! let manifest = Mutex::new(manifest);
//! let versions = Versions(AtomicU64::new(0));
//! let flush = FlushState::new(FlushConfig::new(size));
//! let stores = Stores {
//!     pool: &pool,
//!     table: &table,
//!     segments: &segments,
//!     values: &values,
//!     log: &log,
//!     checkpoints: &checkpoints,
//!     manifest: &manifest,
//!     versions: &versions,
//!     page_values: None,
//! };
//! let write_back = WriteBack::new(stores, &flush, size)?;
//! write_back.adopt(None)?;
//!
//! // Commit 1 wrote page 7, commit 2 wrote it again, and write-back put
//! // an image of each into a segment, which is full and sealed.
//! let page = PageId::new(7).expect("not zero");
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let mut writer = segments.create()?;
//! segments.commit(&mut manifest.lock().expect("the manifest"))?;
//! let first = writer.append(page, BlockContent::Base, Compression::Raw, &[1; 4096])?;
//! table.push(page, Link::base(version(1), first))?;
//! let second = writer.append(page, BlockContent::Base, Compression::Raw, &[2; 4096])?;
//! table.push(page, Link::base(version(2), second))?;
//! writer.seal()?;
//! drop(writer);
//! versions.0.store(2, Ordering::SeqCst);
//!
//! // Both blocks have a link: the first is what a reader at version 1 is
//! // given. Nothing is dead yet.
//! let state = CleanerState::new(CleanerConfig::new());
//! let cleaner = Cleaner::new(write_back, &state);
//! let space = cleaner.space()?;
//! assert_eq!((space.live_bytes(), space.dead_bytes()), (8208, 0));
//!
//! // No reader can ask for version 1 any more: a pass trims its link,
//! // finds the segment half dead, moves the live image to a segment of
//! // its own, and deletes the old segment. Here nothing has to be waited
//! // for: no operation is running, and no table checkpoint is in force.
//! let pass = cleaner.pass()?;
//! assert_eq!((pass.links_trimmed, pass.blocks_moved), (1, 1));
//! assert_eq!((pass.retired, pass.dropped, pass.waiting), (1, 1, 0));
//! let moved = table.newest(page).and_then(|link| link.location()).expect("a block");
//! assert_ne!(moved.file, second.file);
//! let mut buf = Vec::new();
//! assert_eq!(segments.read(moved, 4096, &mut buf)?.payload()?, &[2; 4096][..]);
//! let space = cleaner.space()?;
//! assert_eq!((space.segments.len(), space.dead_bytes()), (1, 0));
//! # Ok(())
//! # }
//! ```
//!
//! # What is live
//!
//! **A block is live while a link of the page table points to it, and dead
//! otherwise** (CL1). The table keeps the link of a block for as long as a
//! reader at the oldest pinned version or later can be given it, so this
//! is the liveness of `transactions.md`, by its simple test (PV3): a block
//! that only a pinned version needs has its link, and is live.
//!
//! The accounting is a read of the page table and keeps no count of its
//! own between passes. A count that is kept up with every write goes
//! wrong where a link is refused, a recovery is cut short or a session
//! ends between a block and its link, and each of those leaves blocks
//! that no chain points to. Read from the table, they are dead because
//! nothing says otherwise: the blocks that the session before wrote after
//! its last table checkpoint, and the segments of a recovery that failed,
//! are found and deleted with no rule of their own. [`Space`] is what one
//! read found, for each segment: the bytes of its blocks, the live ones
//! among them, and those of the live ones that a version pin alone keeps
//! ([`SegmentSpace::pinned_bytes`], the cost of PN7).
//!
//! **The dead space of a store is the dead bytes in a hundred of the
//! bytes of all its blocks** ([`Space::dead_percent`]). File headers and
//! the footers of sealed segments are not counted on either side: no
//! cleaning gives them back. A segment that waits to be deleted counts,
//! with every byte dead.
//!
//! The read takes the table chain by chain while the store is in use
//! ([`PageTable::scan`]). What it relies on is that links into a segment
//! that takes no more blocks only ever go: it looks at which segments
//! have a writer first, and a segment without one that the read finds no
//! link into is one that nothing points into, then and afterwards.
//!
//! # Trimming
//!
//! A pass trims every chain in which a trim removes a link
//! (`page-table.md` T11): the chains of freed pages, which go whole once
//! no reader below the freeing version is left, and the chains an earlier
//! session left with links under a newer base image. Write-back trims the
//! chains it gives a base image itself.
//!
//! **The version it trims at is one that no version pin and no running
//! operation reads below.** The table asks for the first alone. But an
//! operation that holds no pin reads the latest version of its beginning
//! (RD4), and a link it has not looked up yet is no location it has read:
//! were the chain of a page that a commit freed meanwhile trimmed at
//! once, the operation would find no such page. So the cleaner asks its
//! three questions here as it does for a file (PN6): the oldest pin, then
//! an epoch, and the answer is used once every operation guard of before
//! that epoch is released.
//!
//! The chains are trimmed through [`WriteBack::trim_chains`], between two
//! passes of write-back. A trim can take the free link of a page away,
//! and that link is what tells a pass of write-back that a page version
//! it listed as unwritten is of a page that was freed since
//! (`page-table.md` T18). Trimmed while a pass is under way, the chain of
//! a freed page could get a block again.
//!
//! # Moving
//!
//! While the dead space is above [`CleanerConfig::target_dead_percent`],
//! a pass moves the live blocks out of the segments that take no more
//! blocks, the one with the largest share of dead bytes first among
//! those there is room for, until deleting them would bring the dead
//! space within the target, or until its copies come to
//! [`CleanerConfig::max_pass_bytes`]. The segments that already wait to
//! be deleted are left out of that sum, and so are those that cannot be
//! read: what is dead in them stays whatever is moved, and counted, it
//! would have the blocks of every other segment moved for nothing.
//!
//! **A pass works in rounds, and each round moves no more than can wait
//! for one table checkpoint.** Until a segment is deleted the store holds
//! what was moved out of it twice, and a segment that the table
//! checkpoint in force points into is deleted only after the next one.
//! So a round chooses segments whose blocks, with those of the segments
//! that wait to be deleted already, are at most
//! [`CleanerConfig::checkpoint_dead_percent`] of the bytes of all blocks
//! as the round read them, or one segment if nothing waits. A segment
//! that does not fit is passed over for one that does. Then it has a
//! checkpoint taken if that much waits for one, deletes what the
//! checkpoint sets free, and the next round reads the table again. A pass
//! ends when a round moves nothing, at the throttle, or after a few
//! rounds. What waits for a running operation takes that room too: while
//! it waits, no more is moved than the room allows.
//!
//! A block is moved in three steps: it is read, which verifies its
//! checksum; its payload is appended to the cleaner's segment as a block
//! of the same kind; and the link is pointed at the copy
//! ([`PageTable::relocate`]). **Nothing a reader is given changes** (CL2):
//! the copy holds the same payload under the same link, with the same
//! version. A block that was superseded and still has its link is moved
//! like any other, and stays superseded. **If the link is gone when it is
//! to be switched**, because a trim took it while the block was copied,
//! the table refuses, and the copy is dead: a block that left its chain is
//! never brought back (CL4).
//!
//! A reader that took the old location from the table before the switch
//! reads the old block, which is there until its segment is deleted.
//!
//! **A move adds no link.** It changes where one link of a chain points,
//! in one step of the table, and leaves the link's version and kind and
//! the order of the chain as they are. So a page that a commit or the
//! reclaimer of a dropped subtree frees while one of its blocks is copied
//! stays freed: its free link is still the newest of its chain, the link
//! that was moved is one that readers below the freeing version read by,
//! and an id whose page was freed before any block held it gets nothing
//! that says it is in use. Write-back, which adds links, has to look
//! again before it pushes one; the cleaner has nothing to look at again.
//!
//! The cleaner's segment is a segment like write-back's: it takes blocks
//! once a manifest names it, the sync before a table checkpoint is named
//! covers it, and it is sealed at [`CleanerConfig::segment_len`]. The
//! copies are durable before anything depends on them, because a segment
//! is deleted only under a table checkpoint that was named after them.
//!
//! # Deleting
//!
//! A segment is deleted in two steps (CL3, PN6).
//!
//! It is **retired** when it takes no more blocks and the page table
//! points into it no more: a read of the table found no link, or the pass
//! moved its last live block. From then on no operation can find a
//! location in it. The epoch of the clock of retirements is started at
//! that moment ([`Pins::next_epoch`]), after the links are gone.
//!
//! It is **taken out of the store**, and its file deleted, when all of
//! this holds:
//!
//! | Condition | Why |
//! | --- | --- |
//! | Every operation guard that was held when it was retired has been released ([`Pins::oldest_guard`]). | An operation may have read a location in it and not yet read the block (PN5). A version pin alone keeps nothing here: what a pin needs has its link. |
//! | Neither the table checkpoint in force, nor one whose installation left the manifest in doubt, points into it ([`FlushState::checkpoint_segments`]). | A recovery starts from that checkpoint and reads the blocks it points to (`page-table.md` K6). The copies of moved blocks are in no checkpoint until the next one is named. |
//!
//! The answer about the table checkpoint waits for a checkpoint that is
//! being taken, and one that is begun later copies a table that points
//! into no retired segment. The drop and the manifest that no longer
//! names the segment are made under the manifest's lock, and so is the
//! deletion of the file. **A checkpoint of the database that is being
//! created** holds that lock while it samples the manifest and links the
//! segments it names (CK3, `lignindb::checkpoint`), so a segment is
//! deleted before the sample, and is not in it, or after the links are
//! made. The cleaner needs to know of no checkpoint. A finished one has
//! its own links to the files (CK7): the bytes of a segment that the
//! cleaner deletes stay on disk until the checkpoint goes too. The
//! cleaner only reads a segment that it cleans, and writes to a segment
//! only at its end: the copies to its own segment, and the footer that
//! seals one. So what a checkpoint reads through its link does not
//! change.
//!
//! **Waiting is not enough when nothing takes a table checkpoint.** A
//! pass asks write-back for one ([`WriteBack::checkpoint`]) when the
//! segments that only the checkpoint in force keeps hold
//! [`CleanerConfig::checkpoint_dead_percent`] of the bytes of all blocks,
//! and when the dead space is above its target and what waits leaves no
//! room for the segment that is most worth moving. A checkpoint that is
//! put off, because a write transaction holds back a page that has to be
//! in it, is not asked for again in the pass. What waits keeps its room
//! until one is taken: meanwhile only a segment that fits beside it is
//! moved, as while a running operation keeps what waits.
//!
//! **A segment is not cleaned while it is written.** The one that
//! write-back fills holds what the pages it wrote first have left behind
//! since, and the one that takes the cleaner's copies holds copies that
//! were superseded. A pass has such a segment sealed
//! ([`WriteBack::seal_segment`]) once it holds
//! [`CleanerConfig::min_segment_len`] and either its dead bytes are
//! [`CleanerConfig::seal_dead_percent`] of the bytes of all blocks, or
//! its blocks are [`CleanerConfig::max_segment_percent`] of them: a
//! segment that is a large part of the store takes as large a part again
//! while it is moved. So a store is held to the target once its blocks
//! are large against that length, whatever length a segment is sealed at
//! when nobody asks. A value-log file that is being filled is closed by
//! the same rule ([`WriteBack::close_value_file`]).
//!
//! # The value log
//!
//! [`Cleaner::pass_values`] is the cleaner's pass over the value log, in
//! the order [`crate::values`] asks for (CL5). It drops the files that
//! are empty for every reader; if the dead space that a cleaning can
//! give back is above [`CleanerConfig::value_dead_percent`], it cleans
//! the closed file with the largest share of dead bytes, unless less
//! than one byte in a hundred of that file is dead and the target is not
//! zero ([`ValueStore::copy_live`], a sync, a manifest,
//! [`ValueStore::install`]); and it makes the log durable and asks which
//! files are empty from now on ([`ValueStore::retire`]). Those are
//! dropped once the operations that are running then have ended: in the
//! same pass if none is, and by a later pass otherwise. The owners read
//! pages, so the pass takes an operation guard for the cleaning of the
//! file, and gives it back before it asks: a guard of its own over the
//! whole pass would keep the file it has just emptied.
//!
//! A file that is empty and waits to be deleted, for a pin on a version
//! that still points into it or for a checkpoint's hold, is dead space
//! that no cleaning gives back ([`ValueSpace::waiting_bytes`]). It is
//! left out when the dead space is held against its target, as a segment
//! that waits is.
//!
//! A move is refused while a write transaction holds a private copy of
//! the page (PV7), and the copy that was appended for it is dead. A file
//! whose cleaning moved nothing for that reason is set aside, for longer
//! the more files are set aside with it. So once each of the files that a
//! long transaction keeps busy has been tried, about one pass in eight
//! appends values again for nothing, however many those files are, and
//! the passes between are for the files that can be cleaned.
//!
//! A cleaning rewrites pointers in the pool, and the pages it changed are
//! unwritten until write-back's next pass. Until then the block that the
//! segments hold of such a page points into the file that was cleaned,
//! which may be gone. No reader is given that block: the page is in the
//! pool, and a crash recovers the rewrite from the log, which was made
//! durable before the file went.
//!
//! The owners of values are the layers' above, and there are two kinds:
//! [`BothOwners`] makes one [`ValueOwners`] of them.
//!
//! # The throttle
//!
//! A pass over the segments writes at most
//! [`CleanerConfig::max_pass_bytes`] and one block more, counting the
//! copies whose links were gone when they were to be switched, and reads
//! the page table a few times at most. A pass over the value log cleans
//! one file, and the loop sleeps [`CleanerConfig::idle`] between two
//! turns. A pass that moved nothing on, with no segment waiting to be
//! deleted and no trim waiting for an operation, is remembered: the next
//! one does not read the page table if it finds the store as that pass
//! left it. It looks at a few counts and at the list of segments. The
//! bytes the cleaner writes are counted for the store
//! ([`Stats::count_cleaner_bytes`]).
//!
//! What the target costs is the other side of it: the lower
//! [`CleanerConfig::target_dead_percent`] is, the more live bytes are
//! moved for each dead byte that is given back.
//!
//! # Locks
//!
//! The cleaner has one lock of its own, which makes its passes one at a
//! time. It is taken before any lock of write-back or of the stores, and
//! nothing that holds one of those calls the cleaner. Under it a pass
//! takes, one after another and never one inside the other: write-back's
//! lock on its checkpoints (for the segments of the checkpoint in force,
//! and for a checkpoint it asks for), write-back's lock on the files it
//! appends to (for a trim, and for the sealing of a segment), and the
//! manifest (level 3 of `transactions.md` LT7). It takes no latch and no
//! page step: a pointer rewrite takes them in [`ValueStore::install`].
//! The value log takes its lock on the counts for each question the
//! cleaner asks it, and gives it back before it answers.
//!
//! **No reader and no committer waits for the cleaner** (CL7). A reader
//! takes none of those locks. A committer waits for write-back's turns
//! when write-back is over its bounds, and the cleaner holds write-back's
//! files for one footer, or for the table's own steps on a thousand
//! chains: the chains that a large drop left are trimmed a thousand at a
//! time, with a pass of write-back let in between.
//!
//! # When something fails
//!
//! No failure loses a block that a link points to. A pass that failed can
//! be made again.
//!
//! | What fails | What is left |
//! | --- | --- |
//! | A live block that is damaged: its checksum, its frame or its payload | The block and its link, as they were. The segment is left alone until the store is opened again ([`CleanerError::Block`], [`CleanerError::Payload`], [`CleanerStats::segments_unreadable`]): it can never be emptied, so moving the rest of it would gain nothing. |
//! | A read of a live block that the file layer failed | The block and its link, as they were. The pass ends with the error, and a later pass reads the block again: nothing says that the block is damaged. |
//! | An append to the cleaner's segment, or its sealing | What the segment holds is synced, and the next copy goes to a new segment. The block that was being moved is where it was. |
//! | A sync of the cleaner's segment | Links point to copies that cannot be made durable. The segment's writer is kept, so that they stay readable, and write-back installs no table checkpoint until the store is opened again, so no segment that a recovery needs is deleted ([`CleanerError::SegmentNotDurable`]). |
//! | The manifest that stops naming a dropped segment | The segment is out of the store. Its file is deleted by a later replacement of the manifest or by the next session. |
//! | A table checkpoint the cleaner asked for | The checkpoint in force stays, and the retired segments wait. |
//! | The read of the footer of a sealed segment | Nothing of the segment: no reader uses the footer. The accounting takes the end of the file for the end of its blocks, and the segment is cleaned like any other. |
//! | A cleaning of a value-log file | What [`crate::values`] says: the rewrites that were installed stand, each counted, and the other copies are dead space. |
//!
//! After a crash nothing of the cleaner is left to undo. The page table
//! of the next session is the one of the table checkpoint the manifest
//! names, which points only to blocks of segments the manifest names, and
//! the log does not hold a location. A copy that no checkpoint points to
//! is dead space in its segment, which the next session cleans.
//!
//! # What the caller does
//!
//! - **Whoever opens the store** makes a [`CleanerState`] beside
//!   write-back's state and runs [`Cleaner::run`] on a thread, with a
//!   closure that makes the owners of values and calls
//!   [`Cleaner::pass_values`]. It hands over the store's [`Stats`]
//!   ([`Cleaner::with_stats`]).
//! - **Whoever closes it** stops the loop ([`Cleaner::stop`]) and calls
//!   [`Cleaner::finish`], before write-back's own.
//! - **A checkpoint of the database** (TXN-06) holds the manifest's lock
//!   while it samples the manifest and links the segments, and its hold
//!   on the files of the value log
//!   ([`ValueStore::hold_files`](crate::values::ValueStore::hold_files))
//!   until those are linked. It asks nothing else of the cleaner.
//! - **Whoever answers for the pins** ([`Published`]) gives true lower
//!   bounds. The cleaner uses each answer as the bound it is and does not
//!   assume that a later one is higher.
//!
//! # What it does not do
//!
//! - It does not reclaim page ids or the frames the pool holds for
//!   readers: the allocator and the pool do, when the engine asks them.
//! - It removes nothing of a dropped subtree from a page. The tree
//!   layer's reclaimer does (CL6), and the pages it frees are trimmed
//!   here like any freed page.
//! - It does not consolidate chains: write-back does. A chain is moved
//!   block by block, as it is.
//! - It does not move a block that only a version pin keeps to another
//!   place than a block that is current, and it does not wait for a pin:
//!   every block with a link is moved alike. A pin that is held for long
//!   costs the space of what it keeps, which the accounting reports.
//!
//! [`PageTable::scan`]: crate::table::PageTable::scan
//! [`PageTable::relocate`]: crate::table::PageTable::relocate
//! [`WriteBack::trim_chains`]: crate::flush::WriteBack::trim_chains
//! [`WriteBack::checkpoint`]: crate::flush::WriteBack::checkpoint
//! [`WriteBack::seal_segment`]: crate::flush::WriteBack::seal_segment
//! [`WriteBack::close_value_file`]: crate::flush::WriteBack::close_value_file
//! [`FlushState::checkpoint_segments`]: crate::flush::FlushState::checkpoint_segments
//! [`Published`]: crate::flush::Published
//! [`Pins::next_epoch`]: crate::alloc::Pins::next_epoch
//! [`Pins::oldest_guard`]: crate::alloc::Pins::oldest_guard
//! [`ValueStore::copy_live`]: crate::values::ValueStore::copy_live
//! [`ValueStore::install`]: crate::values::ValueStore::install
//! [`ValueStore::retire`]: crate::values::ValueStore::retire
//! [`ValueOwners`]: crate::values::ValueOwners
//! [`Stats`]: crate::stats::Stats
//! [`Stats::count_cleaner_bytes`]: crate::stats::Stats::count_cleaner_bytes

mod clean;
mod config;
mod error;
mod owners;
mod run;
mod space;
mod state;
mod values;

#[cfg(test)]
mod tests;

pub use clean::{Cleaner, Pass};
pub use config::{CleanerConfig, CleanerStats, DEFAULT_PASS_BYTES, DEFAULT_TARGET_DEAD_PERCENT};
pub use error::CleanerError;
pub use owners::{BothOwners, FoundBy};
pub use space::{SegmentSpace, Space, ValueSpace};
pub use state::CleanerState;
pub use values::ValuePass;
