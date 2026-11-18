//! The page table: for every page id, where the page is.
//!
//! - [`PageTable`] holds, for each page, the chain of segment blocks that
//!   make it up: a base image and the delta runs after it, each with the
//!   commit version it brings the page to.
//! - [`TableCheckpoints`] writes a checkpoint of the table to `table/`, has
//!   the manifest name it, and deletes the checkpoints the manifest no
//!   longer names. [`PageTable::load`] reads the one a manifest names.
//! - A checkpoint also carries a [`CountSample`]: a sample of the value
//!   log's counts of live values, which this module stores and gives back
//!   and does not read.
//!
//! The bytes of a checkpoint are `lignin-format`'s:
//! `docs/lignindb/page-table.md` specifies them, and the rules of this
//! module with them. The table knows nothing of what a block holds, and it
//! reads no block.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     manifest::Manifest,
//!     segment::{BlockContent, Compression},
//!     PageSize,
//!     CommitVersion, PageId,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     segment::{ManifestStore, SegmentStore},
//!     table::{CountSample, Link, LogDurable, PageTable, TableCheckpoints},
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let empty = Manifest::new(PageSize::DEFAULT, Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs.clone(), dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//! let checkpoints = TableCheckpoints::open(&manifest)?;
//! let mut writer = segments.create()?;
//! segments.commit(&mut manifest)?;
//!
//! // Commit 1 wrote page 7, and commit 2 changed it. Each block that is
//! // written back becomes a link of the page's chain.
//! let page = PageId::new(7).expect("not zero");
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let table = PageTable::new();
//! let image = writer.append(page, BlockContent::Base, Compression::Lz4, &[0x5A; 4096])?;
//! table.push(page, Link::base(version(1), image))?;
//! let change = writer.append(page, BlockContent::Delta, Compression::Raw, b"a delta")?;
//! table.push(page, Link::delta(version(2), change))?;
//!
//! // A reader at version 1 is given the image, a reader at 2 the image and
//! // the delta to apply to it.
//! let mut chain = Vec::new();
//! assert_eq!(table.read(page, version(1), &mut chain), Some(version(1)));
//! assert_eq!(chain, [image]);
//! assert_eq!(table.read(page, version(2), &mut chain), Some(version(2)));
//! assert_eq!(chain, [image, change]);
//!
//! // A checkpoint that covers version 2: it is written, and then the
//! // manifest names it. The log must be durable through every version the
//! // checkpoint holds, here 2. Installing it syncs the segments first.
//! // It also holds a sample of the value log's counts, which is empty
//! // here: there is no value-log file.
//! let written = checkpoints.write(&table, version(2), &CountSample::default())?;
//! let log = LogDurable {
//!     version: version(2),
//!     place: None,
//! };
//! checkpoints.install(&written, log, &mut manifest, &segments)?;
//!
//! // The next session loads it from the manifest.
//! let (loaded, info) = PageTable::load(&*vfs, dir.path(), manifest.manifest()?)?;
//! assert_eq!(info.expect("a checkpoint").covers(), version(2));
//! assert_eq!(loaded.read(page, version(2), &mut chain), Some(version(2)));
//! assert_eq!(chain, [image, change]);
//! # Ok(())
//! # }
//! ```
//!
//! # Chains
//!
//! A page's chain is a list of [`Link`]s, from the oldest to the newest.
//! Each link has the commit version it brings the page to, and is one of
//! three things:
//!
//! | Link | Says |
//! | --- | --- |
//! | base | The block holds the whole page as that version left it. |
//! | delta | The block holds the changes that bring the page from the link before it to that version. |
//! | free | The commit of that version freed the page. No block. |
//!
//! The rules on a chain, which every change keeps:
//!
//! - The versions never go down along the chain. Two links may have the same
//!   version.
//! - A delta follows a base or a delta. A free link is followed, if by
//!   anything, by a base.
//!
//! A reader at a version is given the newest link that is not above its
//! version, and the links back to the nearest base: [`PageTable::read`]. If
//! that newest link is a free link, or there is none, the page has no
//! version there.
//!
//! **The table holds what was written back.** A change that is only in the
//! log and in the buffer pool has no link. So a link's version is the
//! version at which the page version it completes was born, and the chain
//! of a page read at a version gives the newest page version at or before
//! it that is in a segment.
//!
//! | Change | By | What it does |
//! | --- | --- | --- |
//! | [`PageTable::push`] | write-back, and a commit that frees a page | Adds the newest link. |
//! | [`PageTable::consolidate`] | consolidation | Adds a base image right after the link it was made through, which is named by its location. Later links stay in front of it. |
//! | [`PageTable::relocate`] | the cleaner | Points one link at a copy of its block. Fails if the link is gone, so a superseded block is never brought back. |
//! | [`PageTable::trim`] | whoever knows the oldest version a reader can ask for | Removes the links no reader at that version or later needs, and reports them. The table remembers the newest free link it removed. |
//! | [`PageTable::discard_after`] | recovery | Removes the links above a version. There are none if the log was kept ahead of the checkpoint. |
//!
//! The table trusts its callers on what a block holds: that a base image
//! holds the page as its version left it, that a delta run holds every
//! change since the link before it, and that a copy is a copy.
//!
//! # Lookup and size
//!
//! The newest link of each page is in an array indexed by the page id
//! minus one, so a lookup is one indexed read: no key is hashed or
//! compared, and the cost does not depend on how many pages there are. The
//! array is made of chunks of 4096 slots, which are allocated when a page
//! in them first gets a link, so growing the table moves nothing. The older
//! links of a chain are in an arena, each naming the next older one, and
//! following a chain reads one of them for each link.
//!
//! A link takes [`PageTable::LINK_BYTES`], 28 bytes, in memory, and 28 bytes
//! in a checkpoint. A table of one million pages with one link each is
//! 28.1 MB in memory and 28.0 MB on disk.
//!
//! # A checkpoint, and what it covers
//!
//! A checkpoint is a copy of every chain. The manifest names one
//! checkpoint, with the commit version it **covers**.
//!
//! A checkpoint that covers version *C* is, with the log groups after *C*,
//! enough to rebuild every page: every change that a commit up to *C* made
//! to a page is in a block that one of its links points to. Recovery
//! replays from the group of *C* + 1, and nothing in the log at or before
//! *C* is needed again. The caller of [`TableCheckpoints::write`] vouches
//! for that.
//!
//! A checkpoint can hold links of versions above *C*: blocks that were
//! written back before it was taken. [`CheckpointInfo::reaches`] is the
//! newest of them. Because every link says which version it brings its page
//! to, replay can tell what a page already has: it applies to a page only
//! the changes of versions above the page's newest link
//! ([`PageTable::newest`]).
//!
//! A checkpoint can also lack a page that was in use: one that a commit
//! freed, and whose chain was trimmed away with its free link. The table
//! remembers the newest version of such a free link
//! ([`PageTable::trimmed_free`]), and the checkpoint is named only once the
//! log is durable through it, so no recovery lands where the page is still
//! in use.
//!
//! A checkpoint is written while the table changes. Each chain is copied
//! as it is at one moment, and different chains at different moments. Since
//! a chain only ever gains newer links and loses ones no reader needs, each
//! copy still holds every change up to *C*.
//!
//! # The sample of the value counts
//!
//! The value log's count of live values is recovered with the pages: from a
//! sample of the counts, and the changes in the log after the sample's
//! place ([the value log](crate::values)). The sample is stored in the
//! checkpoint, so that the checkpoint a manifest names is also the sample a
//! recovery uses.
//!
//! Whoever takes a checkpoint takes the sample from the value log, with the
//! place the log had reached ([`LogPlace`]), and passes it to
//! [`TableCheckpoints::write`]. Whoever recovers gets it back in
//! [`CheckpointInfo::sample`] and hands it to the value log. The sample does
//! not have to be of the moment any chain was copied: the place alone says
//! which changes the counts include. A database with no value-log file has
//! an empty sample, and a manifest that names no checkpoint has none.
//!
//! A checkpoint is named only once the log is durable up to the place of
//! its sample ([`PageTableError::LogBehindSample`]).
//!
//! # The life of a checkpoint
//!
//! | Step | What happens |
//! | --- | --- |
//! | Write | [`TableCheckpoints::write`] creates `table/<id>.pt` under an id above every id used before, appends the links, the counts of the sample and the end record, and syncs the file. |
//! | Install | [`TableCheckpoints::install`] checks that the log is durable through all the checkpoint needs ([`CheckpointInfo::log_needed`]), syncs the segments, replaces the manifest with one that names the checkpoint and the segments as they are then, and deletes the checkpoint that was named before. |
//!
//! Between the two the caller makes the log durable (see below). A
//! checkpoint that is written and not installed is deleted by
//! [`TableCheckpoints::discard`], or by the next session.
//!
//! The manifest is replaced only if it vouches for every block the
//! checkpoint points to: it names the block's segment with a valid length
//! at or past the block's end. That is what a location stored in another
//! file needs (the segment store's rule), and it is checked against the
//! manifest that is being installed, whoever replaced the manifest in
//! between. A checkpoint also never covers less than the one it replaces.
//!
//! # What holds after a crash
//!
//! Given the file layer's contract ([`lignin_io::vfs`]) and the segment
//! store's, after a crash at any point:
//!
//! - `MANIFEST` names the checkpoint of the last [`TableCheckpoints::install`]
//!   that returned, or the one an install was naming when the crash came.
//! - That checkpoint's file is in `table/`, whole, and loads, with the
//!   sample of the value counts that was written with it.
//! - Every block a link of it points to is in a segment the manifest names,
//!   below the segment's valid length, with the bytes that were appended.
//! - Any other file in `table/` with a checkpoint's name holds nothing the
//!   database refers to. [`TableCheckpoints::open`] removes it.
//!
//! # When the file layer fails
//!
//! | Failed | Left behind |
//! | --- | --- |
//! | [`TableCheckpoints::write`] | No new checkpoint: its file is removed. The id is not used again in this session. |
//! | [`TableCheckpoints::install`], before the rename of the manifest | The old manifest and its checkpoint, in force. The new checkpoint can be installed again or discarded. |
//! | [`TableCheckpoints::install`], in the rename or the sync of the directory | The old manifest or the new one, whole, with its checkpoint. Neither checkpoint file is touched until the manifest has been opened again and [`TableCheckpoints::manifest_replaced`] called. |
//! | The deletion of the checkpoint that was replaced | The new manifest, in force, and the old file, which the next call or the next session deletes ([`PageTableError::NotDeleted`]). |
//! | [`PageTable::load`] | Nothing changed. |
//!
//! # What the caller does
//!
//! - It holds the directory's lock while a [`TableCheckpoints`] of the
//!   directory exists, and opens it after the manifest.
//! - It passes [`TableCheckpoints::write`] a version that the table covers.
//! - It keeps the log ahead of the checkpoint. Between `write` and
//!   `install` it syncs the log through [`CheckpointInfo::log_needed`], and
//!   it tells `install` how far the log is durable ([`LogDurable`]). A
//!   checkpoint that needs more is refused ([`PageTableError::LogBehind`]),
//!   because a recovery could then land below one of its links, on a page
//!   version its chain no longer gives, or below the commit that freed a
//!   page it has no link of.
//! - It takes the sample of the value counts for a checkpoint after the
//!   sample of every checkpoint before it, syncs the log up to the sample's
//!   place before `install`, and keeps the log from the place of the
//!   checkpoint in force on.
//! - Once a commit has pushed the free link of a page, it writes back
//!   nothing older of that page: the table would refuse the link. Readers
//!   below the freeing version are served from the buffer pool, and no
//!   recovery lands there with this table.
//! - It deletes a segment, or lets the manifest name it shorter, only when
//!   neither the table nor the checkpoint in force points into it:
//!   [`CheckpointInfo::segments`] lists what a checkpoint points into.
//! - After [`SegmentStoreError::ManifestInDoubt`] it opens the manifest
//!   again and calls [`SegmentStore::manifest_replaced`] and
//!   [`TableCheckpoints::manifest_replaced`] with it.
//!
//! [`SegmentStoreError::ManifestInDoubt`]: crate::segment::SegmentStoreError::ManifestInDoubt
//! [`SegmentStore::manifest_replaced`]: crate::segment::SegmentStore::manifest_replaced

mod chains;
mod checkpoint;
mod error;
mod page_table;

#[cfg(test)]
mod tests;

pub use checkpoint::{CheckpointInfo, CountSample, LogDurable, TableCheckpoints};
pub use error::{LinkError, PageTableError};
pub use lignin_format::table::{LogPlace, ValueCount};
pub use page_table::{Link, LinkKind, PageTable};
