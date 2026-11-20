//! Recovery: how a store is opened (STORE-07).
//!
//! [`recover`] takes the manifest of a database directory and returns the
//! store that the directory holds, open, as of one committed version
//! ([`Recovered`]). It is how every session begins, whatever ended the one
//! before: a store that was closed, one whose process was killed, one that
//! lost power, a new store that is its first manifest and nothing else,
//! and a checkpoint that shares its files with a live database.
//!
//! - It loads the page-table checkpoint the manifest names.
//! - It replays the log on it, up to the last commit whose group the log
//!   holds whole, and so brings forward the pages, the counts of the value
//!   log and the page ids.
//! - It names every file of the earlier sessions closed, and turns the log
//!   to a file of the new session.
//!
//! `docs/lignindb/transactions.md` has the rules it keeps (DU2 to DU5,
//! FL4, FL7, ID4), `page-table.md` those of a checkpoint (K1 to K15) and
//! `log-format.md` those of the log's items (LG13 to LG32). The other
//! modules of this crate each say what they ask of whoever recovers, and
//! this one is who that is. It knows no tree, key or hash: a page is what
//! the page codecs make of a logged run.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     delta::Delta,
//!     manifest::Manifest,
//!     tree_page::{CellContent, CellValue, TreeCell, TreePage},
//!     wal::GroupWriter,
//!     CommitVersion, FormatVersion, PageId, PageSize, SlotId,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     load::{LoadBuffers, PageLoader, FRAME_FORMAT},
//!     recovery::{recover, RecoveryOptions},
//!     segment::ManifestStore,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let size = PageSize::DEFAULT;
//! let options = RecoveryOptions {
//!     pool_capacity: 8 << 20,
//!     ..RecoveryOptions::default()
//! };
//!
//! // A new store is its first manifest, and is opened like any other.
//! let empty = Manifest::new(size, Default::default(), None, Vec::new())?;
//! let manifest = ManifestStore::create(vfs.clone(), dir.path(), empty)?;
//! let store = recover(manifest, &options)?;
//! assert_eq!(store.landed(), CommitVersion::EMPTY);
//!
//! // The session's first commit makes a page with one cell. Its group
//! // reaches the log, and then the process is killed: nothing was synced,
//! // and nothing was written back.
//! let page = PageId::new(7).expect("not zero");
//! let at = store.log.next_group()?;
//! let mut group = Vec::new();
//! let mut writer = GroupWriter::new(at.version(), at.address(), &mut group)?;
//! writer.tree_run_with(page, Some(0), |run| {
//!     let cell = CellContent {
//!         key: b"key",
//!         feature: &[],
//!         value_hash: &[1; 32],
//!         kv_hash: &[2; 32],
//!         left: None,
//!         right: None,
//!         hosted_roots: &[],
//!         value: CellValue::Inline(b"value"),
//!         fragment_root: true,
//!     }
//!     .encode()?;
//!     run.push(&Delta::Insert {
//!         slot: SlotId::MIN,
//!         cell: TreeCell::decode(&cell, FormatVersion::CURRENT)?,
//!     })
//! })?;
//! writer.finish();
//! store.log.append_group(&at, &group)?;
//! drop(store);
//!
//! // The next session replays the group. The store is at commit 1, and
//! // the page is in the pool, for readers and for write-back.
//! let store = recover(ManifestStore::open(vfs, dir.path())?, &options)?;
//! assert_eq!(store.landed(), at.version());
//! assert_eq!((store.report.commits, store.report.pages_rebuilt), (1, 1));
//! let loader = PageLoader::new(&store.pool, &store.table, &store.segments, size)?;
//! let frame = loader.get(page, store.landed(), &mut LoadBuffers::new())?;
//! let tree = TreePage::from_validated(&frame, FRAME_FORMAT)?;
//! assert_eq!(tree.cell(SlotId::MIN).expect("the cell").key(), b"key");
//! assert_eq!(store.pool.oldest_unwritten(), Some(at.version()));
//! # Ok(())
//! # }
//! ```
//!
//! # What a recovery does
//!
//! | Step | What happens | By |
//! | --- | --- | --- |
//! | 1 | The table checkpoint the manifest names is loaded: the chains, the version *C* it covers, and the sample of the value log's counts with its place in the log. A manifest that names none gives an empty table, no sample and *C* = 0. | [`PageTable::load`] |
//! | 2 | The segments, the value log and the table checkpoints are opened. Each adopts the files of the earlier sessions closed and read-only, and removes the files in its directory that the manifest does not name. | [`SegmentStore::open`], [`ValueStore::recover`], [`TableCheckpoints::open`] |
//! | 3 | The log files the manifest names are read from the first, item by item, to the first item that is not whole, and each item is replayed (below). | [`LogReader`] |
//! | 4 | What the end of the log says is checked against the checkpoint (below, "What is refused"). The version recovery **lands on**, *Lr*, is that of the last group. | |
//! | 5 | The log is opened where it was found to end, and held from the place of the checkpoint's sample. | [`LogStore::open`] |
//! | 6 | The pages that replay rebuilt and still keeps go into the buffer pool, each as the current version of its page, with the bytes of its pending values. | [`BufferPool::install`] |
//! | 7 | The counts of the value log and the page ids of *Lr* are made. | [`ValueRecovery::finish`], [`alloc::Recovery::finish`] |
//! | 8 | The manifest is replaced: it names every segment and value-log file of the earlier sessions closed. | [`SegmentStore::describe`], [`ValueStore::describe`], [`ManifestStore::update`] |
//! | 9 | The log is made durable to its end and turned to the first file of this session, under a manifest that names every earlier log file closed where its last whole item ends. From here no later recovery reads a byte behind that end (DU3), and the counts that were recovered are durable. The file the log turns to after that one is left to the session. | [`LogStore::begin`], [`ValueStore::mark_durable`] |
//!
//! **It lands on one committed version** (DU2): *Lr* is at or after the
//! last commit that was durable and at or before the last one whose group
//! reached the log. Every page, count and page id is as that commit left
//! it, with every background change whose record the log holds.
//!
//! **It never appends to a file of an earlier session, and never cuts
//! one** (M16, B11). What it writes goes to files of its own: a segment,
//! if replay had more pages than it keeps, and the session's log file. A
//! log file whose end the manifest did not vouch for is written again
//! under its name by the log, as a new file, so whoever holds the old one
//! as a hard link keeps it byte for byte. That is what lets a checkpoint
//! share files with the live database, and a checkpoint's manifest names
//! its log files closed, so nothing behind their valid length is read.
//!
//! # What replay does with an item
//!
//! | The item | For its pages | For the counts of the value log | For the page ids |
//! | --- | --- | --- | --- |
//! | The group of a commit at or before *C* | Nothing: every chain holds what it did (K1). | Its released and acquired pointers, if the group is after the place of the sample. | Nothing. |
//! | The group of a commit after *C* | Each run is applied to its page, unless the page's newest link is of that version or a later one: then the page has it (K3). A run that makes its page needs no page. A freed page gets its free link. | The same. | Every page with a run was written, and every freed page freed (LG15). |
//! | The record of a background change before the place of the sample | Nothing: the chain that the checkpoint copied holds it (K13, K14). It is looked at as a record after the place is, for a page of which replay has what no copied chain holds (below). | Nothing: the sample counts it. | A page that a reclaiming step freed, of those pages, is freed. |
//! | The record of a background change after that place | It is skipped for a page whose newest link is of a later version than the last group before the record. Otherwise it is made exactly when the page still has what the record says it had: the cell holds the place the value is moved from, the cells a reclaiming step takes out are there, or the page it frees is not freed (LG28). | What it releases and acquires, whatever became of its change to the page (DU5). | A page that a reclaiming step freed is freed. |
//! | A checkpoint mark | The one that ends at the place of the sample must cover *C* (LG29). | | |
//!
//! With no checkpoint named, every group is after *C* and every record is
//! after the place.
//!
//! **A change with no image is dropped** (K11). The checkpoint can lack a
//! page that was freed and trimmed away, while the log still holds what
//! commits after *C* did to it before it was freed. Such a change has
//! nothing to apply to. It is dropped, and replay goes on to what frees
//! the page or makes it again: a later commit, or the reclaiming step that
//! freed it, whose record is durable before a checkpoint that lost the
//! page is named (K14). A page that is left with a dropped change when
//! the log ends is damage.
//!
//! **A background change before the place is made to a page that the
//! copied chains do not account for.** K13 has replay look at no record
//! before the place, because the chain that was copied holds its change.
//! That is so for a page that was in use when its chain was copied (K1).
//! A page that was freed since, and whose chain the table trimmed away
//! before the copy (K10), has no chain. If the checkpoint covers a version
//! below the one that freed the page, the log holds what commits after *C*
//! did to the page, and replay meets it with nothing from the checkpoint:
//!
//! - **A page that holds the change of a replayed commit has every
//!   background change after that commit made to it**, by the rule of
//!   LG28. Replay made the commit's change because the chain lacked it
//!   (K3), and a block that held a later background change would hold the
//!   commit too. The commits that follow in the log were made on a page
//!   with those changes: a reclaiming step takes a cell out, and a later
//!   commit puts another into its slot, which does not apply to a page
//!   that still has the cell. This is the page that one of those commits
//!   made.
//! - **A page with a dropped change is freed by the record of the step
//!   that freed it.** A subtree drop frees a page in a page step
//!   (`page-alloc.md` A4), so no commit says that the page is gone, and
//!   K11 would find the dropped change unsettled when the log ends.
//!
//! The counts take nothing from such a record: the sample includes it.
//! `page-table.md` K11 and K13 and `log-format.md` LG28 were written
//! before a page step could free a page or take a cell out, and do not
//! say this.
//!
//! # The pages replay rebuilds
//!
//! A page that replay changes is kept in memory, read once from its chain
//! and changed in place by every later item. When the log ends, each such
//! page is one page version in the pool: born at the newest replayed
//! commit that changed it, unwritten, and current.
//!
//! **A pending value has its bytes with its frame** (FL7). A cell whose
//! value no page step has placed holds the log address of the value.
//! Recovery reads the bytes from the log and attaches them to the frame,
//! because a reader is answered from the frame and never from the log.
//! The log files that hold them stay: their commits are not covered.
//!
//! **Replay keeps a bounded number of pages**
//! ([`RecoveryOptions::max_replay_pages`]), so that a recovery needs no
//! more memory than the session before it had. When more are rebuilt,
//! those without a pending value are written to a segment of this
//! recovery as base images. Each goes the way a commit and write-back
//! take it: installed in the pool, appended, linked in the page table,
//! and marked written, so the pool and the table agree at every moment
//! and the pool can evict the page. A page that is changed again later is
//! read back through its new link. A page with a pending value cannot be
//! written to a segment (PD16) and stays, so what replay holds at any
//! moment is at most the pages that had a pending value when it last
//! wrote, and as many again as it keeps. It tries again once it has
//! replayed as many items as it holds pages, since the steps that place
//! those values are in the log behind them.
//!
//! Such a link is in the page table of this session and in no checkpoint.
//! If the recovery is cut short, the next one starts from the same
//! checkpoint and the blocks are dead space.
//!
//! # What the session is given
//!
//! [`Recovered`] is every part of the store. What a caller has to know of
//! each:
//!
//! - **The log** has turned to this session's first file. The first
//!   commit takes the version after [`Recovered::landed`]: a commit that
//!   the crash took gives its version back (DU3).
//! - **Write-back's state** ([`Recovered::flush`]) is made here, so that
//!   nothing can be appended before it is right. It holds the log from the
//!   place of the checkpoint's sample ([`WriteBack::adopt`] need not be
//!   called). It knows where the group of each replayed commit begins and
//!   how much log no checkpoint covers, so backpressure and the dropping
//!   of log files work from the first moment. And it knows, for each
//!   rebuilt page, the oldest replayed commit whose change the page
//!   holds: **no table checkpoint covers one of those commits before a
//!   block holds the page**, although the pool lists the page under the
//!   newest of them. Write-back is told nothing of the records, so a
//!   rebuilt page is written as a base image.
//! - **The buffer pool** is made here, of the manifest's page size. It
//!   holds the rebuilt pages, which it cannot evict until write-back has
//!   written them, and whatever replay read from segments.
//! - **Segments and value-log files** of this session are created by
//!   write-back when it first needs one.
//! - **Counts start with the session.** The log's counts
//!   ([`LogStore::appended`]) and write-back's are zero. What was replayed
//!   is in [`RecoveryReport`]. What recovery left for write-back to do
//!   shows where an operator looks for it: the replayed log that no
//!   checkpoint covers is in `log_held`, and the rebuilt pages are among
//!   the unwritten ones. The pool's counts of misses include the pages
//!   replay read.
//!
//! # Twice
//!
//! **Replaying twice gives the same state.** A recovery changes nothing
//! that the next one starts from: the manifest names the same table
//! checkpoint, and the log files it names hold the same items. What it
//! adds is the closing of the earlier files, a log file with no item, and
//! perhaps a segment that no checkpoint points into. So a recovery that is
//! killed at any point, or that fails, is followed by one that lands on
//! the same version with the same pages, counts and page ids, and so is a
//! recovery that completed and whose session committed nothing.
//!
//! # What a recovery costs
//!
//! **The time is the log's.** Every item of the log is read once, in the
//! order it was appended, and each change is made in memory to a page that
//! was read once. Nothing is synced while the log is read, unless replay
//! holds more pages than it keeps.
//!
//! **The first log file is read from its first item**, wherever in it the
//! place of the checkpoint's sample is. The commits that the checkpoint
//! does not cover can lie before that place, since a checkpoint taken
//! while commits come in covers less than the log holds at its sample,
//! and nothing durable says where the group of the first of them begins.
//! A reader that began at the place would lose them, and would not see
//! that it had unless a later group followed. What keeps the read short
//! is the writer: write-back turns the log where a checkpoint begins
//! ([`LogStore::rotate_early`]), so that the files with the commits the
//! checkpoint covers can go when it is installed. The file a recovery
//! starts in then holds what was logged while that checkpoint was taken.
//! Where the checkpoint could not cover those commits, or something
//! still held their files, a recovery reads them as well.
//!
//! **Of a segment a recovery reads the file header, and the blocks that
//! links of the page table point to.** The header is read, and the file
//! measured, where the segment is opened, which is done for every
//! segment the manifest names. A block is read where a link points: at
//! the offset and of the length the link gives, checked by its own frame
//! ([`PageLoader`]). A recovery reads no footer of a sealed segment and
//! walks no segment block by block, so nothing in it rests on what a
//! footer's index says of a block (S16 of `segment-manifest.md`).
//!
//! **What it makes durable does not grow with the log.** A recovery of a
//! store that a session closed makes five syncs, in three calls of the
//! file layer, and each is there for something a later step rests on:
//!
//! | Call | What is synced | Why, and why here |
//! | --- | --- | --- |
//! | 1 | The database directory ([`ManifestStore::open`], which the caller makes). | The manifest that was found has to be the one a crash leaves before anything is done by it: an earlier session may have ended between the rename of a manifest and the sync of the directory, and the opens of the stores delete the files this manifest does not name. The same sync makes durable the name of every directory beside the manifest, so the stores sync nothing for a directory of theirs that is there. |
//! | 2 | This session's first log file, `wal/`, and the temporary file of the manifest that names the log file, as one set ([`LogStore::begin`], [`Vfs::sync_together`]). | A manifest that names a file must not be in force before the file and its name are durable, and must not be in force before its own bytes are. None of the three has to come before another, so they are one set, and a file layer whose sync ends in a flush of the whole device makes that flush once. |
//! | 3 | The database directory. | The manifest has its name by a rename, and the log turns to its file only once a crash cannot take the manifest that names it away. |
//!
//! One log file is created, and one manifest written. The file the log
//! turns to after that is made by whoever turns the log in the session
//! ([`WriteBack::roll_log`], and a table checkpoint for its own turn), so
//! the open does not wait for it. On a file system whose syncs are slow
//! these three calls are what a recovery waits for, and not the replay.
//!
//! Three things grow with what the last session left open, each with
//! syncs of its own. If it left segments or value-log files that no
//! manifest names closed, one manifest more closes them, before the
//! log's: the temporary file and the database directory. The part of its
//! last log file that no sync is known to cover is written again, at
//! most one file of [`LogOptions::target_file_len`] bytes (see the log's
//! "A file of an earlier session"): the new file and `wal/`. And the
//! pages replay writes when it holds more than it keeps are appended to
//! a segment that replay creates and has a manifest name, with the syncs
//! of both, and that is synced when it is full and when the log ends.
//!
//! **What it deletes.** The opens of the stores remove the files that the
//! manifest does not name. The log's first manifest of the session no
//! longer names a log file of the earlier sessions that holds nothing of
//! the log, and such a file is deleted once that manifest is in force:
//! it is the only file a recovery drops, since recovery drops no segment
//! and retires no value-log file. **A file that cannot be deleted fails
//! the open**, whichever of the two steps meets it
//! ([`LogError::NotDeleted`] where the manifest has just stopped naming
//! the file, the store's I/O error where it finds the file unnamed). A
//! recovery that fails returns no store, so no store is left with a
//! manifest it was not told of, and what it made durable by then is a
//! state the next recovery starts from. The next open meets the same
//! file where it opens the log, which removes the log files its manifest
//! does not name, and fails on it as well for as long as the file cannot
//! be deleted.
//!
//! # What is refused
//!
//! A directory that is not what a session of this store leaves, whatever
//! the crash, is reported and not repaired ([`RecoveryError::is_damage`]):
//!
//! | What recovery finds | Error | Rule |
//! | --- | --- | --- |
//! | The log ends before the commit the checkpoint covers. | [`RecoveryError::LogBehindCheckpoint`] | K4 |
//! | The log goes on from a later commit than the one after *C*. | [`RecoveryError::CommitsMissing`] | K1 |
//! | The checkpoint holds a link of a later version than *Lr*. Nothing is discarded by its version: the log goes first, and what such a link replaced may be gone. | [`RecoveryError::LinksAfterLanding`] | K4, FL4 |
//! | No mark ends at the place of the sample, or the mark there covers another version. | [`RecoveryError::MarkMissing`], [`RecoveryError::WrongMark`] | LG29 |
//! | A page with a dropped change was not freed or made again by a later commit or reclaiming step. | [`RecoveryError::ChangesWithoutPage`] | K11 |
//! | A reclaiming step frees a page that is in use and does not hold what the step took out of it. | [`RecoveryError::StepOnAnotherPage`] | LG28 |
//! | A logged change does not apply to its page. | [`RecoveryError::Change`] | |
//! | A cell is given a pointer to a value that the value log does not have. | [`RecoveryError::ValueNotThere`] | FL7 |
//! | A pending value is named by the log and not carried in it. No session of this version writes one, because nothing keeps the file the name leads to (LG24). | [`RecoveryError::NamedValue`] | LG23 |
//!
//! A release of a value pointer that no count included is not refused: the
//! value log keeps the file for good, which is its own answer to a count
//! that is wrong, and [`RecoveryReport::miscounts`] says how many records
//! had one.
//!
//! Everything else that fails is the error of the part that failed, with
//! the part named: a file that is gone or damaged, or the file layer.
//!
//! # What the caller does
//!
//! - It holds the directory's lock, and opens the manifest with
//!   [`ManifestStore::open`], which is where it can look at the format
//!   version and the page size before anything is changed. A new store is
//!   made with [`ManifestStore::create`] and recovered like any other.
//! - It runs nothing else on the directory while [`recover`] runs (DU4).
//! - It puts the manifest behind a lock, builds write-back's view of the
//!   stores with [`Recovered::flush`] as its state, and runs the
//!   background loop ([`WriteBack::run`]). What publishes commits starts
//!   at [`Recovered::landed`], with no pin and no guard.
//! - It may call [`WriteBack::consolidate_long_chains`] once, and calls
//!   the reclaims of the page ids and of the pool as after any commit:
//!   the ids of the pages that replayed commits freed are free at the
//!   first one.
//!
//! # What it does not do
//!
//! - It deletes no segment and trims no chain. The blocks that the
//!   session before wrote after its last table checkpoint are in no
//!   chain, and neither are the blocks of a recovery that was cut short:
//!   they are dead space for whoever cleans segments (STORE-08).
//! - It takes no table checkpoint. The first one of the session covers
//!   what was replayed, once write-back has written the rebuilt pages.
//! - It restores no hold on a value-log file for a value that the log
//!   names (LG24), and refuses such a log.
//!
//! [`BufferPool::install`]: crate::cache::BufferPool::install
//! [`alloc::Recovery::finish`]: crate::alloc::Recovery::finish
//! [`ValueRecovery::finish`]: crate::values::ValueRecovery::finish
//! [`ValueStore::recover`]: crate::values::ValueStore::recover
//! [`ValueStore::describe`]: crate::values::ValueStore::describe
//! [`ValueStore::mark_durable`]: crate::values::ValueStore::mark_durable
//! [`SegmentStore::open`]: crate::segment::SegmentStore::open
//! [`SegmentStore::describe`]: crate::segment::SegmentStore::describe
//! [`ManifestStore::update`]: crate::segment::ManifestStore::update
//! [`TableCheckpoints::open`]: crate::table::TableCheckpoints::open
//! [`PageTable::load`]: crate::table::PageTable::load
//! [`LogStore::open`]: crate::wal::LogStore::open
//! [`LogStore::begin`]: crate::wal::LogStore::begin
//! [`LogStore::rotate_early`]: crate::wal::LogStore::rotate_early
//! [`LogError::NotDeleted`]: crate::wal::LogError::NotDeleted
//! [`WriteBack::roll_log`]: crate::flush::WriteBack::roll_log
//! [`Vfs::sync_together`]: lignin_io::Vfs::sync_together
//! [`LogStore::appended`]: crate::wal::LogStore::appended
//! [`ManifestStore::open`]: crate::segment::ManifestStore::open
//! [`ManifestStore::create`]: crate::segment::ManifestStore::create
//! [`WriteBack::adopt`]: crate::flush::WriteBack::adopt
//! [`WriteBack::run`]: crate::flush::WriteBack::run
//! [`WriteBack::consolidate_long_chains`]: crate::flush::WriteBack::consolidate_long_chains

mod error;
mod replay;

#[cfg(test)]
mod tests;

use lignin_format::CommitVersion;

pub use self::error::RecoveryError;
use self::replay::Replay;
use crate::{
    alloc::PageAllocator,
    cache::{BufferPool, PoolConfig},
    flush::{FlushConfig, FlushState},
    load::PageLoader,
    segment::{ManifestStore, SegmentStore},
    table::{CheckpointInfo, PageTable, TableCheckpoints},
    values::{FileCount, ValueStore},
    wal::{LogOptions, LogReader, LogStore, DEFAULT_READ_WINDOW_LEN},
};

/// The size of the buffer pool a store is recovered into unless
/// [`RecoveryOptions::pool_capacity`] says another: 256 MiB.
pub const DEFAULT_POOL_CAPACITY: usize = 256 << 20;

/// How a store is recovered, and what the session that follows is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryOptions {
    /// The size of the store's buffer pool in bytes
    /// ([`PoolConfig::capacity`]). Its frames are of the page size the
    /// manifest records.
    pub pool_capacity: usize,
    /// How the session's log is written.
    pub log: LogOptions,
    /// The thresholds of the session's write-back. `None` is
    /// [`FlushConfig::new`] of the store's page size.
    pub flush: Option<FlushConfig>,
    /// The most pages that replay keeps rebuilt in memory before it writes
    /// those that can be written to a segment. `None` is
    /// [`FlushConfig::max_unwritten`], the number of page versions that a
    /// committer may leave waiting for write-back.
    pub max_replay_pages: Option<usize>,
    /// How many bytes of the log are fetched at a time
    /// ([`LogReader::with_window`]).
    pub read_window: usize,
}

impl Default for RecoveryOptions {
    fn default() -> Self {
        Self {
            pool_capacity: DEFAULT_POOL_CAPACITY,
            log: LogOptions::default(),
            flush: None,
            max_replay_pages: None,
            read_window: DEFAULT_READ_WINDOW_LEN,
        }
    }
}

/// What a recovery did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// The commit version recovery landed on: the last commit whose group
    /// the log holds whole.
    pub landed: CommitVersion,
    /// The commit version the table checkpoint covers, or 0 if the
    /// manifest names none.
    pub covers: CommitVersion,
    /// The items of the log that were read.
    pub items: u64,
    /// The bytes those items take in the log.
    pub log_bytes: u64,
    /// The commits that were replayed: those after the one the checkpoint
    /// covers.
    pub commits: u64,
    /// The records of background changes that were looked at: those from
    /// the place of the checkpoint's sample on, and those before it of
    /// pages that the copied chains do not account for.
    pub changes: u64,
    /// The background changes among them that were made to their page,
    /// which did not have them yet.
    pub steps: u64,
    /// The changes that were dropped because the table checkpoint has no
    /// image of their page (`page-table.md` K11).
    pub changes_dropped: u64,
    /// The pages that replayed commits and page steps freed.
    pub pages_freed: u64,
    /// The pages that were written to a segment while the log was read,
    /// because there were more of them than replay keeps.
    pub pages_spilled: u64,
    /// The pages that were put into the buffer pool for write-back to
    /// write.
    pub pages_rebuilt: u64,
    /// The pending values of those pages, whose bytes were read from the
    /// log and are with their frames.
    pub pending_values: u64,
    /// The records that released a value pointer which no count of the
    /// value log included. The file of such a pointer is kept from now on,
    /// whatever its count says ([`FileCount::miscounted`]).
    pub miscounts: u64,
}

/// A store that was recovered: every part of it, open, as of one committed
/// version.
#[derive(Debug)]
pub struct Recovered {
    /// The manifest, which names every file of the earlier sessions closed
    /// and the first log file of this one.
    pub manifest: ManifestStore,
    /// The buffer pool. It holds the pages that replay rebuilt, each as
    /// the current version of its page, for write-back to write.
    pub pool: BufferPool,
    /// The page table: the chains of the checkpoint, the free links of the
    /// pages that replayed commits freed, and the links of the pages that
    /// were written while the log was read.
    pub table: PageTable,
    /// The segments.
    pub segments: SegmentStore,
    /// The value log, with the counts of live values of the version
    /// recovery landed on.
    pub values: ValueStore,
    /// The log, turned to the first file of this session.
    pub log: LogStore,
    /// The table checkpoints.
    pub checkpoints: TableCheckpoints,
    /// The page ids, as that version has them.
    pub allocator: PageAllocator,
    /// The state of the session's write-back. It keeps the log for the
    /// checkpoint in force and knows what of the replayed commits no block
    /// holds.
    pub flush: FlushState,
    /// The table checkpoint the manifest names, if it names one.
    pub checkpoint: Option<CheckpointInfo>,
    /// What the recovery did.
    pub report: RecoveryReport,
}

impl Default for RecoveryReport {
    fn default() -> Self {
        Self {
            landed: CommitVersion::EMPTY,
            covers: CommitVersion::EMPTY,
            items: 0,
            log_bytes: 0,
            commits: 0,
            changes: 0,
            steps: 0,
            changes_dropped: 0,
            pages_freed: 0,
            pages_spilled: 0,
            pages_rebuilt: 0,
            pending_values: 0,
            miscounts: 0,
        }
    }
}

impl Recovered {
    /// The commit version the store was recovered to: the first commit of
    /// the session takes the version after it.
    pub fn landed(&self) -> CommitVersion {
        self.report.landed
    }
}

/// Recovers the store whose manifest `manifest` is, and returns it open,
/// as of the last commit whose group its log holds whole. See the [module
/// documentation](self) for the steps, for what the session is given and
/// for what the caller does.
///
/// The caller holds the directory's lock, and nothing else works on the
/// directory until this has returned (DU4).
///
/// If it fails, nothing was made durable that the next recovery cannot
/// start from, and it can be called again with the manifest opened anew.
pub fn recover(
    mut manifest: ManifestStore,
    options: &RecoveryOptions,
) -> Result<Recovered, RecoveryError> {
    let named = manifest.manifest()?.clone();
    let vfs = manifest.vfs().clone();
    let dir = manifest.dir().to_path_buf();
    let size = named.page_size();

    let (table, checkpoint) = PageTable::load(&*vfs, &dir, &named)?;
    let segments = SegmentStore::open(&manifest)?;
    let sample: Vec<FileCount> = checkpoint
        .iter()
        .flat_map(|info| &info.sample().counts)
        .map(|count| FileCount {
            file: count.file,
            values: count.values,
            bytes: count.bytes,
            miscounted: count.miscounted,
        })
        .collect();
    let counts = ValueStore::recover(&manifest, &sample)?;
    let checkpoints = TableCheckpoints::open(&manifest)?;
    let pool = BufferPool::new(PoolConfig {
        page_size: size,
        capacity: options.pool_capacity,
    })?;
    let flush = FlushState::new(options.flush.unwrap_or(FlushConfig::new(size)));
    let covers = checkpoint
        .as_ref()
        .map_or(CommitVersion::EMPTY, CheckpointInfo::covers);
    let place = checkpoint.as_ref().and_then(|info| info.sample().place);
    let max_pages = options
        .max_replay_pages
        .unwrap_or(flush.config().max_unwritten);

    let (values, allocator, log, report) = {
        let loader = PageLoader::new(&pool, &table, &segments, size)
            .map_err(|source| RecoveryError::Loader { source })?;
        let mut replay = Replay::new(
            &pool, &table, &segments, &flush, loader, size, counts, covers, place, max_pages,
        );
        let mut reader = LogReader::new(vfs.clone(), &dir, &named).with_window(options.read_window);
        while let Some(replayed) = reader.next_item(|read| replay.item(read, &mut manifest))? {
            replayed?;
        }
        let end = reader.into_end()?;
        let landed = replay.ended(&end)?;

        let log = LogStore::open(&manifest, &end, options.log)?;
        flush.adopt(&log, checkpoint.as_ref())?;
        replay.install_rebuilt(&log)?;
        replay.close()?;
        let (counts, ids, report) = replay.into_parts();
        let values = counts.finish()?;
        let allocator = ids.finish(&table, landed)?;
        (values, allocator, log, report)
    };

    // One manifest says what the segments and the value-log files of the
    // earlier sessions are now.
    manifest.update(|current| -> Result<_, RecoveryError> {
        Ok(values.describe(&segments.describe(current)?)?)
    })?;
    // Neither store has a file to delete here: a recovery drops no
    // segment and retires no value-log file. The log has, and an open
    // that cannot delete it fails ("What it deletes").
    segments.manifest_replaced(&manifest)?;
    values.manifest_replaced(&manifest)?;
    log.begin(&mut manifest)?;
    values.mark_durable(values.applied())?;

    Ok(Recovered {
        manifest,
        pool,
        table,
        segments,
        values,
        log,
        checkpoints,
        allocator,
        flush,
        checkpoint,
        report,
    })
}
