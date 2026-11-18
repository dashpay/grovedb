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
//!     PageId,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::segment::{ManifestStore, SegmentStore};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let empty = Manifest::new(Default::default(), None, Vec::new())?;
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
//! The first manifest of a directory is written the same way, so a directory
//! has no manifest or a whole one. [`ManifestStore::open`] removes a
//! `MANIFEST.tmp` that a crash left behind, and syncs the directory before
//! anything acts on the manifest it read.
//!
//! # The life of a segment
//!
//! | Step | What happens | How manifests name the segment from then on |
//! | --- | --- | --- |
//! | Create | [`SegmentStore::create`] creates `seg/<id>.seg` under an id above every id used before, appends the file header, syncs the file and syncs `seg/`. | Open, 20 bytes valid. |
//! | Name | The manifest is replaced. From here the segment takes blocks. | |
//! | Append | [`SegmentWriter::append`] appends one block. | |
//! | Sync | [`SegmentWriter::sync`] or [`SegmentStore::sync`] syncs the file. | Open, valid up to the last block synced. |
//! | Seal | [`SegmentWriter::seal`] appends the footer and syncs the file. | Sealed, valid to its end. |
//! | Drop | [`SegmentStore::drop_segment`] takes the segment out of the store. | Not named. |
//! | Delete | [`SegmentStore::manifest_replaced`] removes the file, once a manifest that does not name it is durable. | |
//!
//! The store's own record of its segments is what is true now, and a manifest
//! is a durable copy of it. [`SegmentStore::describe`] writes that record
//! into a manifest, and [`SegmentStore::commit`] replaces the manifest with
//! the result. A step that changes how a segment is named takes effect on
//! disk with the next replacement, whoever makes it.
//!
//! A segment that the manifest names was written by an earlier session if
//! this session did not create it. Such a segment is read and never written:
//! its file is opened read-only, and if the earlier session left it open it
//! ends at its valid length (rule M16 of the format).
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
//!   id. An id whose file was created and never named can be used again.
//!
//! # What the caller does
//!
//! - It holds the directory's lock ([`lignin_io::Vfs::lock`]) while a
//!   [`ManifestStore`] or a writable [`SegmentStore`] of the directory
//!   exists.
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

mod error;
mod files;
mod manifest;
mod store;
mod writer;

#[cfg(test)]
mod tests;

pub use error::{ManifestChange, SegmentStoreError};
pub use files::{create_file, FileNames};
pub use manifest::{read_manifest, ManifestStore};
pub use store::{BlockLocation, BlockWalk, SegmentStore};
pub use writer::SegmentWriter;
