//! Write-back: page versions go from the buffer pool to segments
//! (STORE-06).
//!
//! A commit leaves its page versions in the [buffer pool](crate::cache) and
//! its changes in the [log](crate::wal). Write-back is what makes them
//! readable from [segments](crate::segment): it writes each page version
//! as one block, a run of deltas or a base image, links the block in the
//! [page table](crate::table), gives the values that a commit carried in
//! its log group their place in the [value log](crate::values), takes the
//! table checkpoints, and lets go of the log files that nothing needs any
//! more. Whoever commits is slowed when it falls behind.
//!
//! [`FlushState`] is what write-back keeps for one open store. It is made
//! when the store is opened and lives beside the stores. [`WriteBack`] is
//! a view of the stores ([`Stores`]) and that state: a few references,
//! which every thread copies, like a [`PageLoader`](crate::load::PageLoader).
//! The store has no thread of its own: whoever opens it runs
//! [`WriteBack::run`] on one.
//!
//! ```
//! use std::sync::{
//!     atomic::{AtomicU64, Ordering},
//!     Arc, Mutex,
//! };
//!
//! use lignin_format::{
//!     delta::Delta,
//!     manifest::Manifest,
//!     tree_page::{CellContent, CellValue, HashField, TreeCell, TreePageMut},
//!     wal::GroupWriter,
//!     CommitVersion, FormatVersion, PageId, PageSize, SlotId,
//! };
//! use lignin_io::StdVfs;
//! use lignin_store::{
//!     alloc::{Epoch, Pins},
//!     cache::{BufferPool, OwnerId, PoolConfig},
//!     flush::{Checkpointed, FlushConfig, FlushState, Published, Stores, WriteBack},
//!     load::FRAME_FORMAT,
//!     segment::{ManifestStore, SegmentStore},
//!     table::{PageTable, TableCheckpoints},
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
//! // A new store, opened: every part of it, and the state of write-back.
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
//! let state = FlushState::new(FlushConfig::new(size));
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
//! let write_back = WriteBack::new(stores, &state, size)?;
//! // The manifest names no table checkpoint yet.
//! write_back.adopt(None)?;
//!
//! // A commit makes a page with one cell, and a second commit changes a
//! // hash of that cell. Each writes its group to the log, tells
//! // write-back, installs its page and is published.
//! let page = PageId::new(7).expect("not zero");
//! let slot = SlotId::new(1).expect("not zero");
//! for round in 0..2u8 {
//!     let created = round == 0;
//!     let copy = match created {
//!         true => pool.new_private(OwnerId::new(1), page)?,
//!         false => pool.make_private(OwnerId::new(2), page, versions.latest()).expect("there"),
//!     };
//!     let counts = values.step()?;
//!     let at = log.next_group()?;
//!     let mut group = Vec::new();
//!     let mut writer = GroupWriter::new(at.version(), at.address(), &mut group)?;
//!     {
//!         let mut frame = copy.write()?;
//!         if created {
//!             TreePageMut::init(&mut frame, size, page, 0)?;
//!         }
//!         writer.tree_run_with(page, created.then_some(0), |run| {
//!             let mut tree = TreePageMut::open(&mut frame, size, FRAME_FORMAT)?;
//!             let cell = CellContent {
//!                 key: b"key",
//!                 feature: &[],
//!                 value_hash: &[1; 32],
//!                 kv_hash: &[2; 32],
//!                 left: None,
//!                 right: None,
//!                 hosted_roots: &[],
//!                 value: CellValue::Inline(b"value"),
//!                 fragment_root: true,
//!             }
//!             .encode()?;
//!             let delta = match created {
//!                 true => Delta::Insert {
//!                     slot,
//!                     cell: TreeCell::decode(&cell, FormatVersion::CURRENT)?,
//!                 },
//!                 false => Delta::Hash {
//!                     slot,
//!                     field: HashField::Value,
//!                     hash: &[3; 32],
//!                 },
//!             };
//!             delta.apply(&mut tree)?;
//!             run.push(&delta)
//!         })?;
//!     }
//!     writer.finish();
//!     let appended = log.append_group(&at, &group)?;
//!     write_back.committed(at.version(), appended.at, &group)?;
//!     pool.install(&copy, at.version())?;
//!     drop(counts);
//!     versions.0.store(at.version().get(), Ordering::SeqCst);
//!     write_back.place_ready()?;
//!
//!     // Write-back, which a loop on another thread does in a store that
//!     // is in use: the new page goes to a segment as a base image, and
//!     // the changed hash as a run of 39 bytes behind it, in a block of 47.
//!     let pass = write_back.pass()?;
//!     assert_eq!((pass.written, pass.waiting), (1, 0));
//! }
//! let stats = state.stats();
//! assert_eq!((stats.base_blocks, stats.delta_blocks), (1, 1));
//! assert_eq!((stats.delta_bytes, stats.value_bytes), (39, 0));
//!
//! // A table checkpoint covers both commits, and the session ends.
//! let Checkpointed::Installed { covers, .. } = write_back.checkpoint()? else {
//!     unreachable!("nothing waits");
//! };
//! assert_eq!(covers, versions.latest());
//! write_back.finish()?;
//! # Ok(())
//! # }
//! ```
//!
//! # What a pass writes
//!
//! [`WriteBack::pass`] looks at every page version the pool lists as
//! unwritten, page by page, each page's versions oldest first. One block
//! holds one page's base image or one run of its deltas, never two pages.
//!
//! | The page version | What the pass does |
//! | --- | --- |
//! | is the page's current version and holds a pending value | Places the value if it can (below), and otherwise leaves the page for a later pass. No page version with a pending value reaches a segment (`page-delta.md` PD16): the image is checked, and so is the page at the end of every run. |
//! | is superseded, and holds a pending value or bytes attached for one | Keeps it in the pool for the readers that can still ask for it ([`BufferPool::hold_for_readers`](crate::cache::BufferPool::hold_for_readers)), and puts its changes into the block of the next version of the page that is written. This is the choice for a superseded version with a pending value (`transactions.md`, choice 25): the version stays in memory, and its value is placed for the version that carries it on. |
//! | is superseded, and no reader can ask for it: the oldest pin is at or past the version that superseded it | The same: one block for several commits (FL3). A version that a pin can still reach, and that has no pending value, gets a block of its own. |
//! | is older than the free link of its page | Nothing. Nothing older than a free link is written back (`page-table.md` T18), and the pool keeps the version until no reader needs it. |
//! | is of a page that was freed after the pass took the version in hand | Its block may be written, and gets no link. The link is pushed in a page step, and only while the pool still owes that version: a page that was made under an id used before and freed before it had a link has no free link of its own to refuse the block (T18), and its id may name another page by then. |
//! | is a flat page | A base image. Every block of a flat page is its image (PD13). |
//! | is a tree page whose chain ends in a base image or a run | A run of deltas behind that link, if the conditions below hold. Otherwise a base image. |
//! | is a tree page with no chain, or one that ends in a free link | A base image. A run that creates its page is never written to a segment (PD11). |
//!
//! The link of a block is pushed to the page table before the pool is told
//! that the version is written, and the pool is not told when the link was
//! refused, by the table or because the pool owes the version no more: the
//! page was freed meanwhile, and the block is dead space
//! ([`FlushStats::links_refused`]).
//!
//! **A run is written only when it is known to give the page.** Write-back
//! keeps the records of what each commit did to each tree page from the
//! moment it is told of the commit ([`WriteBack::committed`]) until a block
//! holds them, and the records of the page steps that changed the page
//! afterwards ([`WriteBack::append_change`]). A run is the records of every
//! version between the newest link and the version that is written, one
//! after another. Before it is written, it is applied to the page as the
//! chain gives it, and the result must be the image of the page in the
//! frame, byte for byte. If it is not, or if a record is missing, the page
//! is written as a base image ([`FlushStats::runs_refused`]). So what
//! write-back was told decides only how many bytes a page costs, never
//! what a reader gets: a commit or a page step that tells it nothing is
//! still written back correctly.
//!
//! **The thresholds** ([`FlushConfig`]). A page is written as a base image
//! instead of a run, and its chain so consolidated, when the run would be:
//!
//! | Threshold | Default | |
//! | --- | --- | --- |
//! | [`max_chain_deltas`](FlushConfig::max_chain_deltas), the chain-length threshold | 8 | one run more than this behind the newest base image. With 0, every block is a base image. A chain never has more runs behind its base image than this, since the pass that would add one writes the image instead. |
//! | [`max_chain_bytes`](FlushConfig::max_chain_bytes), the byte threshold | half a page | more bytes of delta blocks behind the newest base image than this, as they are stored, frames included. |
//! | [`max_run_len`], the delta-run threshold | one page size | a payload longer than this. It is a rule of the store and no setting. **A loader can refuse a delta block whose payload is longer than [`max_run_len`] of the store's page size**: write-back writes none, and reads none when it verifies a run. |
//! | | | as long as the image itself, or longer. |
//!
//! A chain that an earlier session wrote under other thresholds is brought
//! within them by [`WriteBack::consolidate_long_chains`], which builds each
//! image from the blocks of the chain (`page-table.md` T9).
//!
//! Once a page has a new base image and no reader can ask for a version
//! before it, the pass trims the older links from its chain (T11) and
//! counts their blocks as dead space of their segments
//! ([`FlushState::dead_space`]), for whoever cleans segments (STORE-08).
//!
//! **No reader means no version pin and no running operation.** The table
//! asks for the oldest version a reader can ask for, and the oldest pin
//! alone is not it. An operation that holds no pin reads the latest
//! version of its beginning under its guard, and a link it has not looked
//! up yet is no location it has read (`transactions.md` RD4, PN5): were
//! the chain trimmed at once under a commit that superseded the page
//! meanwhile, the operation would find no version of the page at the
//! version it reads. So a pass puts the three questions as the cleaner
//! does for its own trims (PN6, [`crate::cleaner`]): the oldest pin, then
//! an epoch, and then the oldest guard, which every later pass asks for
//! again. The first pass that finds no operation guard held from before
//! that epoch, which can be the pass that asked, trims at that version,
//! or at a higher answer whose epoch is ready as well. A page waits in a
//! list that write-back keeps from pass to pass until chains are trimmed
//! at the version of its newest image or at a later one.
//!
//! So a trim waits for the operations that held a guard when the epoch
//! was started, and for no other. One that began after the oldest pin
//! was asked for and before the epoch is among them. One that ends
//! before the pass looks at the guards holds nothing back.
//!
//! For a page that a pass gave an image, with the image's commit
//! published and no pin below it, the answer that the pass gets, or one
//! that waits already, reaches the image:
//!
//! - If the pass finds no guard held from before that answer's epoch, it
//!   trims the chain itself.
//! - If it finds one, the first later pass that finds none trims the
//!   chain. That is the next pass when every such operation is over by
//!   then, however closely operations follow one another, and a later
//!   one when such an operation goes on through more passes.
//!
//! A pass that has as many answers waiting as write-back keeps takes no
//! new one: a later pass that has room does. A pin below the image holds
//! the trim back until the pin is gone, and a commit that is not
//! published yet until it is.
//!
//! While a page waits, its chain is trimmed at each version that chains
//! may be trimmed at, once. So a page that gets an image in every pass,
//! as a flat page that every commit changes does, keeps the images from
//! the one that a reader at that version sees and no older ones.
//!
//! **What bounds the wait is what bounds a guard.** A guard is held for
//! one operation, and for as long as a node view that the operation
//! handed out lives (PN5). A guard that is never released stalls these
//! trims as a pin that is never released does, and as it stalls the
//! cleaner's trims and the reuse of page ids: the links stay, and their
//! blocks are not dead space yet, so no segment is cleaned of them.
//! Nothing else waits for a trim. The page is written, no log is held
//! for a link that waits, and [backpressure](#backpressure) counts none
//! of it: a committer never waits for a trim, as it never waits for a
//! pin. What write-back keeps meanwhile is one entry for each page that
//! waits and at most eight answers that wait for their operations.
//!
//! The later pass needs no commit: the background loop makes a turn
//! whenever its idle time has passed ([`WriteBack::run`]), and every turn
//! is a pass. What still waits when the session ends is trimmed by the
//! cleaner of a later one, as every chain is that an earlier session left
//! with links under a newer base image.
//!
//! # Pending values
//!
//! A value that is too long for its cell travels in the log group of its
//! commit and is a pending value of its cell until write-back has placed
//! it (`transactions.md` FL7). A pass places the pending values of each
//! page's current version, in three steps that are one at a time for the
//! store:
//!
//! 1. The value is appended to the value-log file that write-back fills,
//!    with the id of its page, under a hold of its own
//!    ([`ValueHold`](crate::values::ValueHold)). Its bytes come from the
//!    frame, to which the commit attached them, and from the log if the
//!    frame has none ([`LogStore::read_pending`](crate::wal::LogStore::read_pending)).
//!    The values of one page are appended in the order a scan reads them,
//!    if the layer above says what that order is ([`PageValues`]).
//! 2. The file is synced, and a manifest covers the values.
//! 3. A page step gives the cell its pointer: under the lock on the value
//!    counts and the lock of the page, the step checks that the current
//!    version still holds that pending value in that cell and that **no
//!    write transaction holds a private copy of the page** (PV7), appends
//!    its record to the log, puts a frame with the pointer and without the
//!    value's bytes in place of the old one, and counts the value. The
//!    hold ends there.
//!
//! A step that a private copy keeps from being made is made later: by a
//! later pass, or by [`WriteBack::place_ready`], which **whoever commits
//! calls once its commit is published**, when the copies of its
//! transaction are gone. A value whose cell is gone by then, replaced,
//! removed or freed with its page, is given up, and its bytes in the value
//! log are dead space ([`FlushStats::values_given_up`]). A value that a
//! commit carried to another page is pending again there, under another
//! id, and is placed anew.
//!
//! Bytes that are attached to a frame for no cell that is pending are
//! taken off in a page step with no record, since what the page says does
//! not change (PV4).
//!
//! **Regrouping.** Before a pass writes a new base image of the current
//! version of a tree page, it has the layer above regroup the page's
//! values, if it was given what does that ([`PageValues::regroup`]): the
//! values are appended again, back to back in scan order, and each cell is
//! given its new pointer in a page step. The image is then written with
//! those pointers. `lignin-store` knows no tree, so `lignin-tree`
//! implements [`PageValues`] and whoever opens the store hands it over in
//! [`Stores::page_values`].
//!
//! # Table checkpoints and the log
//!
//! [`WriteBack::checkpoint`] takes a table checkpoint in the order
//! `page-table.md` asks for (K1, K4, K14, K15; `log-format.md` LG29):
//!
//! 1. The log is rolled if that is due. Then it is turned to its next
//!    file, if the file it ends in holds a commit, once for each
//!    checkpoint that is installed (below). Then a pass writes back what
//!    it can.
//! 2. Under the lock on the value counts: the version *C* that the
//!    checkpoint covers is fixed, the counts are sampled, the mark is
//!    appended to the log, and the log is held from the end of the mark,
//!    which is the place of the sample. The place of a sample is never
//!    before the place of the sample before it
//!    ([`FlushError::SampleWentBack`]).
//! 3. A second pass writes back what was unwritten at that moment. Every
//!    page that a page step changed before the sample is among it, and
//!    replay looks at no page step before the sample's place. So if one of
//!    those page versions cannot be written yet, the attempt ends here
//!    ([`Checkpointed::PutOff`]), and the checkpoint in force stays.
//! 4. The checkpoint file is written, a copy of every chain.
//! 5. The log is synced, after the copy.
//! 6. The checkpoint is installed, with what that sync made durable. The
//!    segments are synced there, before the manifest names the checkpoint.
//! 7. The log files that lie before both the group of *C* + 1 and the
//!    place of the sample are taken out of the log.
//!
//! ***C* is the newest version such that no change of a commit up to it
//! is still to be written back** when the sample is taken, and never less
//! than what the checkpoint in force covers. A change that is in the pool
//! only, in a version kept for readers, is still to be written back until
//! the block of the next version of its page holds it. A change to a page
//! that was freed since is not (T18). A commit that comes in while the
//! checkpoint is taken is not covered, though its pages may be in it: the
//! next checkpoint covers it.
//!
//! **What step 3 waits for is more than the rule asks.** The rule is about
//! the pages that a page step changed before the sample. The pool does not
//! say which unwritten pages those are, and write-back does not rely on
//! having been told of every step, so a checkpoint waits for every page
//! version that was unwritten at its sample. It is therefore put off
//! while a write transaction holds a private copy of a page whose value
//! waits to be placed. **A table checkpoint needs a moment at which no
//! transaction holds such a page back**, and when blocks are applied back
//! to back there may be none. [Backpressure](#backpressure) makes one.
//!
//! **The log goes first** (FL4). Write-back syncs segments and names them
//! in the manifest whenever it has to, and blocks of commits that are not
//! durable in the log can so become durable. They are reachable only
//! through a link, and a link survives a crash only in a checkpoint, which
//! is named after the log is durable through everything it holds. Recovery
//! needs to discard nothing by its version.
//!
//! **The log's files** are write-back's to keep going
//! ([`WriteBack::roll_log`]): after an append that failed, the log takes
//! nothing until it has been rolled, and no committer does that
//! (`transactions.md` CM9). A turn of the log is made under the lock on
//! the value counts, because it moves the end of the log, which a commit
//! holds still under that lock while it writes its group for a place.
//! Whoever needs the log durable calls [`WriteBack::sync_log`], which also
//! tells the value log how far its counts are durable.
//!
//! **The log is turned where a checkpoint begins**
//! ([`LogStore::rotate_early`](crate::wal::LogStore::rotate_early)). A
//! recovery reads the first log file its manifest names from the first
//! item, and with a checkpoint in force no file before the file of the
//! group of *C* + 1 or of the sample's place, whichever is earlier, is
//! needed for it (step 7). With the log turned before the first pass,
//! every commit up to that moment is in a file before the turn. Where the
//! pass writes them all back, the checkpoint covers them and those files
//! can go with it, and the next recovery then reads what was logged while
//! the checkpoint was taken. Without the turn it reads up to a whole log
//! file of commits that its pages have. The turn is made once for each
//! checkpoint, so the attempts of a checkpoint that is put off do not
//! make a file each. With no commit in the log's file, the checkpoint is
//! taken where the log is, and so it is where the file for the turn
//! cannot be made: the checkpoint does not rest on the turn, and an error
//! of the turn's own does not fail it.
//!
//! **A file for the log to turn to** is made by whoever turns the log,
//! where the log has none: a file is created, and a manifest names it. A
//! recovery leaves that to the session
//! ([`LogStore::begin`](crate::wal::LogStore::begin)), so that an open
//! creates one log file and not two. A checkpoint makes the file for its
//! own turn. [`WriteBack::roll_log`] makes the one it turns to away from
//! a file that is full, and leaves another one ready behind that turn,
//! which the next checkpoint's turn then takes without a sync. None of
//! it is made inside a commit.
//!
//! # Backpressure
//!
//! [`WriteBack::has_room`] says whether write-back is within its bounds,
//! and [`WriteBack::wait_for_room`] makes a committer wait while it is
//! not (`transactions.md` FL5):
//!
//! | Bound | Default | What it bounds |
//! | --- | --- | --- |
//! | [`max_unwritten`](FlushConfig::max_unwritten) | 4096 | the page versions that wait to be written back, which the pool cannot evict |
//! | [`max_pending_bytes`](FlushConfig::max_pending_bytes) | 32 MiB | the bytes of pending values that frames hold |
//! | [`max_log_held`](FlushConfig::max_log_held) | 256 MiB | the log from the oldest commit that no table checkpoint covers, which is what no log file can be dropped from and what a recovery reads ([`WriteBack::log_held`]) |
//!
//! The committer waits for the turns of the background loop
//! ([`WriteBack::tick`]): a pass, and a table checkpoint if one is due,
//! which it is whenever more log is held than the bound. The wait ends
//! when write-back is within its bounds again, or when a turn has moved
//! nothing on ([`Room::Stalled`]): what is left then waits for something
//! that cannot happen during the wait, such as the end of a transaction
//! that holds a private copy.
//!
//! **Where the committer waits decides what the wait can do.** Before step
//! 2 of CM3 it holds the copies of its transaction, and what they hold
//! back stays. So **a committer that applies blocks back to back calls
//! [`WriteBack::wait_for_room`] after [`WriteBack::place_ready`], when its
//! commit is published and before its next transaction copies a page.**
//! It returns at once while write-back is within its bounds. Once it is
//! not, the turn the committer waits for finds no page held back: it
//! places every value, writes every page and takes the checkpoint. That
//! wait is what keeps two things bounded when blocks come back to back:
//!
//! - **The log that is held.** With no such moment no checkpoint is
//!   installed, however far write-back has written.
//! - **A page that every block copies and that gets a new value out of
//!   line in every block.** It has a pending value whenever a pass looks
//!   at it, since the value of the last block cannot be placed under the
//!   copy of the next. Each value is placed one block late, by
//!   [`WriteBack::place_ready`], and the page itself is never written, so
//!   no checkpoint can cover its commits.
//!
//! The tests in `tests/measure.rs` measure both on blocks of the reference
//! workload's shape: with the call to [`WriteBack::place_ready`] every
//! value is placed one block after its own, and with the wait the log
//! that is held stays within its bound.
//!
//! # When something fails
//!
//! No failure loses anything that was committed: a page version that was
//! not written back is still in the pool and in the log, and a checkpoint
//! that was not installed leaves the one before it in force. A call that
//! failed can be made again.
//!
//! **An error that is about one page does not end a pass.** The other
//! pages are written, and the pass returns the first such error when it
//! is done. The page stays unwritten, so no checkpoint covers its commit,
//! and the log keeps it.
//!
//! | What fails | What write-back does |
//! | --- | --- |
//! | The append of a page step's record to the log | Nothing is installed. If the append was taken back, the log is settled before the value is offered again ([`LogStore::settle`](crate::wal::LogStore::settle)). The value keeps its hold and stays ready, and the step is made once the log has been rolled. If the log cannot find out what became of the append, it takes nothing more in this session: the call fails with that error, and the value keeps its hold, because the record may be in the log. |
//! | An append to a segment, or the sealing of one | What the segment holds is synced, and the next block goes to a new segment. The page stays unwritten. |
//! | A sync of a segment, write-back's or the cleaner's | The blocks that were not durable cannot be made durable any more, and chains point to them. The segment's writer is kept, so that they stay readable, and **no table checkpoint that begins afterwards is installed until the store is opened again** ([`FlushError::SegmentsNotDurable`]). Readers, commits and write-back go on, and the log keeps every commit since the checkpoint in force. |
//! | An append to a value-log file | The file is given up, and the next value goes to another file ([`FlushStats::value_files_given_up`]). What the file holds is synced by its writer, through the handle that appended it, and the writer is dropped after that sync has returned ([`ValueWriter::give_up`](crate::values::ValueWriter::give_up)). The file is then closed where it ends: whoever cleans the value log takes closed files only, and takes this one in the same session. The values that went into the file before the one that was refused are durable there, and are placed as any value is: once a manifest covers them, and where no private copy holds their page step back. If the sync fails, the next row holds. |
//! | A sync of a value-log file | The file is given up as in the row above, with no further sync of it in this session: it is closed at the length that the last sync that succeeded had made durable. The values that were appended behind that length are given up with their holds, and a later pass appends them to another file. No manifest covers them, and a cell is given its pointer only behind a manifest that covers the value, so no cell points behind that length. It is no sync of the log or of a segment, and the session goes on. |
//! | A sync of the log | Nothing more becomes durable in this session, which is the log's rule (`wal`), so no checkpoint is installed. |
//! | The start record of the file that a table checkpoint turns the log to | Where the log finds that the record is not whole in the file, it stays in the file it was in, and the checkpoint goes on there. Where the file layer refused the record and the record is in the file, the log has turned, to a file that takes no item. Before its mark the checkpoint rolls a log that has no file that takes items, as a log is rolled after any append that failed, three times at most: what stands in the way of a roll is the checkpoint's error, and so is a roll whose every turn ended in such a file. A log that has no such file after the last roll fails the mark. In both cases the error is what the file layer answered the append that left the log so ([`LogError::NoActiveFileSince`](crate::wal::LogError::NoActiveFileSince)), so that a checkpoint which a full device keeps from its mark fails for room by name. |
//! | The replacement of a manifest, left in doubt | The manifest is opened again by the next call that needs it, and every store is told which one is in force, each whatever another one answers: the call ends with the first error once all four have been told, and the manifest is not in doubt after it. If the manifest cannot be opened again, no store is told and the doubt stays. A checkpoint whose installation was left in doubt keeps its hold on the log beside the one that was in force, and [`FlushState::in_force`] gives the one of before, until a call of [`WriteBack::checkpoint`] that is given the manifest without an error finds out from it which of the two it names: the call that settles the manifest is not that call if a store answers it with an error. |
//! | A block of a chain cannot be read when a run is verified | The pass ends with the error. The page stays unwritten. |
//! | A pending value has no bytes of its length, or a page is not what its format allows | The page stays as it is, and the pass goes on with the others. |
//! | What regroups a page's values | The steps that were made stand. The page is written as it is, nothing more is regrouped in that pass, and the pass returns [`FlushError::Regroup`]. |
//!
//! [`WriteBack::run`] counts a turn that fails, passes the error to its
//! caller and goes on. **After a turn that failed it waits before the
//! next, whoever asks for a turn** ([`FlushConfig::wait_after_failure`];
//! `disk-full-and-backup.md`, DF12). A turn makes its pass whatever became
//! of the log before it, so a committer that waits for room is never left
//! waiting: after a turn that failed, the turn it waits for is the one
//! after that wait.
//! `tests/faults.rs` fails every file operation of write-back in turn,
//! `tests/crash.rs` cuts the power after every change to what a crash can
//! leave, and `tests/threads.rs` freezes the files, as a crash would leave
//! them, while several threads commit, write back and take checkpoints.
//!
//! # Locks
//!
//! Write-back takes its own locks and the stores' in one order, which is
//! `transactions.md` LT7 with write-back's own in between:
//!
//! | Order | Lock | Held for |
//! | --- | --- | --- |
//! | 1 | the checkpoints of the session | one table checkpoint |
//! | 2 | the files write-back appends to | one pass, one consolidation |
//! | 3 | the manifest (level 3 of LT7) | one replacement, and one roll of the log |
//! | 4 | the values that are ready for their step | one call that places values |
//! | 5 | the value counts (level 4a) | one page step, one sample with its mark, one turn of the log |
//! | 6 | a page's step (level 5) | the checks of one step, the append of its record, and the swap of the frame; or one look at the page's unwritten versions, or at whether it has a current version |
//! | 7 | what write-back was told and has not written; the log's append lock | a few instructions; nothing is taken under either |
//!
//! No file is synced and no manifest is replaced under the lock on the
//! value counts or under a page's step. [`WriteBack::new`] takes no lock,
//! so a view can be made under any of them.
//!
//! # What the caller does
//!
//! **Whoever opens the store** takes the [`FlushState`] that
//! [recovery](crate::recovery) made for it and builds the [`Stores`]. It
//! implements [`Published`] for what publishes commits and keeps the pins,
//! and hands over the tree layer's [`PageValues`] if it has one. It runs
//! [`WriteBack::run`] on a thread, and may call
//! [`WriteBack::consolidate_long_chains`] once.
//!
//! **Recovery** makes the state before anything is appended to the log:
//!
//! - It keeps the log for the checkpoint the manifest names, as
//!   [`WriteBack::adopt`] does. A store that is put together without a
//!   recovery, as the example above is, calls that itself.
//! - It says where the group of each commit it replayed begins, and how
//!   much log those commits take, so that the log that is held is known
//!   from the start and the log files of the session before are dropped
//!   once a checkpoint covers what they hold.
//! - It says, for each page version it put into the pool, which is the
//!   oldest replayed commit whose change the page version holds. The pool
//!   lists such a page under the newest of those commits, and no block
//!   holds any of them: **the version a table checkpoint covers stays
//!   below the oldest until a block holds the page**, as it does for a
//!   page version that is kept for its readers.
//! - It tells write-back nothing of the records, so a page that it rebuilt
//!   is written as a base image.
//! - A page version that it rebuilt with a pending value has the value's
//!   bytes attached to its frame, read from the log (FL7).
//!
//! **A commit** (TXN-05):
//!
//! - takes the lock on the value counts before it asks the log for the
//!   place of its group and holds it until the group is in (`wal`,
//!   "Locks");
//! - calls [`WriteBack::committed`] once the group is in the log and
//!   **before it installs its first page**, and goes on whatever it
//!   returns;
//! - calls [`WriteBack::place_ready`] once it is published, and after a
//!   rollback, and then [`WriteBack::wait_for_room`], before its thread's
//!   next write transaction copies a page; it may call
//!   [`WriteBack::wake`] so that its pages are written at once;
//! - calls [`WriteBack::sync_log`], and not the log's own sync, where its
//!   sync policy or `flush()` asks for the log to be durable.
//!
//! **A page step of anyone else** (the value log's cleaner, a regrouping,
//! the removal of what a dropped subtree left) appends its record through
//! [`WriteBack::append_change`] where it would call the log.
//!
//! **Whoever cleans segments** ([`crate::cleaner`], STORE-08) takes the
//! live blocks from the page table. It trims the chains of freed pages and
//! those an earlier session left, which a pass does not, through
//! [`WriteBack::trim_chains`], between two passes. It drops a segment that
//! [`FlushState::checkpoint_segments`] does not list, has the segment and
//! the value-log file that are being filled closed when they hold too
//! much that is dead ([`WriteBack::seal_segment`],
//! [`WriteBack::close_value_file`]), and asks for a table checkpoint
//! ([`WriteBack::checkpoint`]) when segments wait for one.
//!
//! **Whoever closes the store** stops the loop ([`WriteBack::stop`]) and
//! calls [`WriteBack::finish`].
//!
//! # What write-back does not do
//!
//! - It does not reclaim the page versions it held for readers:
//!   [`BufferPool::reclaim`](crate::cache::BufferPool::reclaim) is called
//!   by whoever knows the pins and the guards. Until then such a version
//!   counts among the pending bytes.
//! - It moves no block from one segment to another and deletes no segment
//!   (STORE-08), and it does not recover ([`crate::recovery`]).
//! - It does not rescue the blocks of a segment whose sync failed by
//!   writing their pages again. The session goes on without checkpoints,
//!   and the next open recovers from the log.
//! - It takes no checkpoint while a write transaction holds back a page
//!   that was unwritten when the sample was taken, and it cannot make a
//!   long transaction end. While one is open, the log is held.

mod checkpoint;
mod config;
mod error;
pub(crate) mod page;
mod place;
mod run;
mod state;
pub(crate) mod trim_bound;
mod write_back;

#[cfg(test)]
pub(crate) mod tests;

pub use checkpoint::Checkpointed;
pub use config::{
    max_run_len, DeadSpace, FlushConfig, FlushStats, DEFAULT_CHAIN_DELTAS, DEFAULT_SEGMENT_LEN,
};
pub use error::FlushError;
pub use run::{Room, Tick};
pub use state::FlushState;
pub use write_back::{PageValues, Pass, Published, RegroupError, Stores, WriteBack};
