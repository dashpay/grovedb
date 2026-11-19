//! The segment store: the segment files of a database directory, and the
//! manifest that names them.
//!
//! - [`ManifestStore`] reads `MANIFEST` and replaces it atomically.
//! - [`SegmentStore`] holds the segments a manifest names. It reads a block
//!   by its location, reads the footer of a sealed segment, walks a segment
//!   that has none, creates segments and deletes them.
//! - [`SegmentWriter`] appends blocks to one segment and seals it.
//!
//! The bytes are `lignin-format`'s: `docs/lignindb/segment-manifest.md`
//! specifies them. This module decides which file operations happen and in
//! which order. It knows nothing of what a block holds.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     manifest::Manifest,
//!     segment::{BlockContent, Compression},
//!     PageSize,
//!     PageId,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::segment::{ManifestStore, SegmentStore};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let empty = Manifest::new(PageSize::DEFAULT, Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs, dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//!
//! // A new segment takes blocks once the manifest names it.
//! let mut writer = segments.create()?;
//! segments.commit(&mut manifest)?;
//! let page = PageId::new(7).expect("not zero");
//! let image = [0x5A; 4096];
//! let location = writer.append(page, BlockContent::Base, Compression::Lz4, &image)?;
//!
//! let mut buf = Vec::new();
//! let block = segments.read(location, image.len(), &mut buf)?;
//! assert_eq!(block.payload()?, image.as_slice());
//!
//! // Sealing appends the footer; the next manifest names the segment sealed.
//! writer.seal()?;
//! segments.commit(&mut manifest)?;
//! let index = segments.index(location.file, &mut buf)?;
//! assert_eq!(index.blocks().map(|block| block.page).collect::<Vec<_>>(), [page]);
//! # Ok(())
//! # }
//! ```
//!
//! # Replacing the manifest
//!
//! [`ManifestStore::update`] takes four steps:
//!
//! 1. write the new manifest to a new file, `MANIFEST.tmp`
//! 2. sync that file
//! 3. rename it to `MANIFEST`
//! 4. sync the database directory
//!
//! A crash at any point leaves the old manifest or the new one, whole:
//!
//! | Crash | `MANIFEST` afterwards |
//! | --- | --- |
//! | before step 3 | The old one, which was not touched. `MANIFEST.tmp` may exist, with any content. |
//! | from step 3 until step 4 returns | The old one or the new one. The file layer keeps a rename whole or not at all, and the new file was synced before it was renamed. |
//! | after step 4 | The new one. |
//!
//! A replacement is refused if it breaks a rule against the manifest it
//! would replace ([`ManifestChange`]). One of those rules is that the page
//! size never changes: the first manifest of a directory sets it, for tree
//! pages and flat pages alike, and a page image does not hold it. Whoever
//! opens a store with a page size of its own calls
//! [`ManifestStore::check_page_size`] before it reads a page.
//!
//! The first manifest of a directory is written the same way, so a directory
//! has no manifest or a whole one. [`ManifestStore::open`] removes a
//! `MANIFEST.tmp` that a crash left behind, and syncs the directory before
//! anything acts on the manifest it read.
//!
//! # The life of a segment
//!
//! | Step | What happens | How manifests name the segment from then on |
//! | --- | --- | --- |
//! | Create | [`SegmentStore::create`] creates `seg/<id>.seg` under an id above every id used before, appends the file header, syncs the file and syncs `seg/`. Then the segment enters the store, and its id with it. | Open, 20 bytes valid. |
//! | Name | The manifest is replaced. From here the segment takes blocks. | |
//! | Append | [`SegmentWriter::append`] appends one block. | |
//! | Sync | [`SegmentWriter::sync`] or [`SegmentStore::sync`] syncs the file. | Open, valid up to the last block synced. |
//! | Seal | [`SegmentWriter::seal`] appends the footer and syncs the file. | Sealed, valid to its end. |
//! | Drop | [`SegmentStore::drop_segment`] takes the segment out of the store. | Not named. |
//! | Delete | [`SegmentStore::manifest_replaced`] removes the file, once a manifest that does not name it is durable. | |
//!
//! A checkpoint that is being created needs the files of the manifest it
//! sampled until it has linked them (CK3 of `docs/lignindb/transactions.md`).
//! It takes [`SegmentStore::hold_files`] in its sampling step: while the
//! hold lives no segment is dropped, and one that was dropped before is not
//! deleted.
//!
//! The store's own record of its segments is what is true now, and a manifest
//! is a durable copy of it. [`SegmentStore::describe`] writes that record
//! into a manifest, and [`SegmentStore::commit`] replaces the manifest with
//! the result. A step that changes how a segment is named takes effect on
//! disk with the next replacement, whoever makes it.
//!
//! A segment and its id enter that record in one step, once the file is
//! whole and durable, and segments enter it in the order of their ids: one
//! is created at a time. A manifest written while the file is still being
//! made neither names the segment nor records its id. So when a manifest
//! comes to name the segment, its id is still above every segment id the
//! manifest before it records. If a manifest recorded the id first, the one
//! that came to name the segment would be refused, because a newly named
//! file must have an id above the last one recorded for its kind
//! ([`ManifestChange::FileIdReused`]).
//!
//! A segment that the manifest names was written by an earlier session if
//! this session did not create it. Such a segment is read and never written:
//! its file is opened read-only, and if the earlier session left it open it
//! ends at its valid length (rule M16 of the format). The first manifest this
//! session writes names it closed at that length, which is how rule M15 has
//! the files of an earlier session named.
//!
//! # What holds after a crash
//!
//! Given the file layer's contract ([`lignin_io::vfs`]), after a crash at any
//! point:
//!
//! - `MANIFEST` is whole. It is the manifest of the last replacement that
//!   returned, or of one that was in progress.
//! - Every segment it names is in `seg/`, and the bytes below the segment's
//!   valid length are the ones that were appended: the file header, whole
//!   blocks and, if it is named sealed, its footer. A manifest never gives a
//!   segment a valid length beyond what was synced.
//! - A file in `seg/` that it does not name holds nothing the database
//!   refers to. [`SegmentStore::open`] removes it.
//! - A segment id that ever held a block is never given to another file,
//!   because a segment takes blocks only once a durable manifest records its
//!   id. An id that no durable manifest records can be given out again by a
//!   later session: its file, if a crash or a failed creation left one,
//!   holds no block and is removed when the store is opened.
//!
//! # When the file layer fails
//!
//! An operation that the file layer fails returns that error in a
//! [`SegmentStoreError`], and leaves this:
//!
//! | Failed | Left behind |
//! | --- | --- |
//! | [`ManifestStore::update`], before its rename | The old manifest, in force. The update can be made again. |
//! | [`ManifestStore::update`], in its rename or the sync of the directory | The old manifest or the new one, whole. The [`ManifestStore`] answers [`SegmentStoreError::ManifestInDoubt`] until the manifest is opened again. |
//! | [`SegmentStore::create`] | No segment in the store. Its id is not used again in this session, and no manifest records it. If the file it began could not be removed, the next session removes it. |
//! | [`SegmentWriter::append`], or the footer of [`SegmentWriter::seal`] | The segment's blocks, ending where they did. |
//! | A sync of a segment | The segment as it was last synced, which is all a manifest names of it. It takes no more blocks ([`SegmentStoreError::SegmentFailed`]). |
//! | The deletion of a dropped segment's file | The new manifest, in force, and the file, which the next call or the next session deletes ([`SegmentStoreError::NotDeleted`]). |
//!
//! # What the caller does
//!
//! - It creates the database directory and makes that name durable, by
//!   syncing the directory above it, before the first manifest is written. A
//!   crash takes a directory whose name was not synced, with everything in
//!   it.
//! - It holds the directory's lock ([`lignin_io::Vfs::lock`]) while a
//!   [`ManifestStore`] or a writable [`SegmentStore`] of the directory
//!   exists.
//! - It gives a [`SegmentStore`] the [`ManifestStore`] it was opened from, or
//!   one opened again on the same directory through the same file layer. The
//!   store checks that a manifest is of its directory by the path and by
//!   nothing else ([`SegmentStoreError::WrongDirectory`]): two file layers
//!   that both have the path are the caller's to keep apart.
//! - After every [`ManifestStore::update`] that returns `Ok`, it calls
//!   [`SegmentStore::manifest_replaced`]. [`SegmentStore::commit`] does both.
//! - It stores a [`BlockLocation`] in another file only under a manifest that
//!   gives the block's segment a valid length at or past the block's end.
//!   So before a manifest names a page-table checkpoint, it calls
//!   [`SegmentStore::sync`] and passes that manifest through
//!   [`SegmentStore::describe`]. Blocks past a segment's valid length can be
//!   read until the session ends, and not after it.
//! - After [`SegmentStoreError::ManifestInDoubt`] it opens the manifest
//!   again, which settles which manifest is durable, and calls
//!   [`SegmentStore::manifest_replaced`] with it.
//!
//! # Files of the other kinds
//!
//! [`FileNames`], [`list_or_create_dir`] and [`create_file`] name, list and
//! create the numbered files of all four kinds, for the stores of the other
//! three. A store that gives out ids keeps the rule that
//! [`SegmentStore::create`] keeps. It lets a manifest record an id as the
//! last of its kind only when the file is one the manifest names, or one the
//! store would have a manifest written now name, and it brings its files
//! into that record in the order of their ids. A manifest that records an
//! id before its file can be named uses the id up: [`ManifestStore::update`]
//! refuses the manifest that names the file afterwards.

mod error;
mod files;
mod manifest;
mod store;
mod writer;

#[cfg(test)]
pub(crate) mod tests;

pub use error::{ManifestChange, SegmentStoreError};
pub use files::{create_file, list_or_create_dir, FileNames};
pub use manifest::{read_manifest, ManifestStore};
pub use store::{BlockLocation, BlockWalk, Extent, FileHold, SegmentStore};
pub use writer::SegmentWriter;
