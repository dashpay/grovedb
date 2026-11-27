//! Page materialisation: a page version that is in no frame is built from
//! its blocks and put into one.
//!
//! - [`PageLoader`] is the read path of a store: the page in the buffer
//!   pool, or, on a miss, the page read from its segments into a frame.
//! - [`LoadBuffers`] is the memory a load reads into, kept by the caller
//!   from one load to the next.
//! - [`LoadError`] is why a page was refused.
//!
//! A load takes the page's chain from the page table (`page-table.md` T5):
//! the base image and the delta runs after it. It reads each block, which
//! verifies the block's checksum, copies the image into a frame, applies the
//! runs to it in order, and hands the frame to the pool. No hash is
//! computed: every hash in the frame is the hash a block held.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     delta::{Delta, DeltaRunWriter},
//!     manifest::Manifest,
//!     segment::{BlockContent, Compression},
//!     tree_page::{CellContent, CellValue, HashField, TreePage, TreePageMut},
//!     CommitVersion, PageId, PageSize, SlotId,
//! };
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     cache::{BufferPool, PoolConfig},
//!     load::{LoadBuffers, PageLoader, FRAME_FORMAT},
//!     segment::{ManifestStore, SegmentStore},
//!     table::{Link, PageTable},
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let size = PageSize::DEFAULT;
//! let empty = Manifest::new(size, Default::default(), None, Vec::new())?;
//! let mut manifest = ManifestStore::create(vfs, dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//! let mut writer = segments.create()?;
//! segments.commit(&mut manifest)?;
//! let table = PageTable::new();
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let (page, slot) = (PageId::new(7).expect("not zero"), SlotId::MIN);
//!
//! // Commit 1 wrote the page, and write-back stored its image.
//! let mut frame = vec![0; size.get()];
//! let mut written = TreePageMut::init(&mut frame, size, page, 0)?;
//! written.insert(
//!     slot,
//!     &CellContent {
//!         key: b"key",
//!         feature: &[],
//!         value_hash: &[1; 32],
//!         kv_hash: &[2; 32],
//!         left: None,
//!         right: None,
//!         hosted_roots: &[],
//!         value: CellValue::Inline(b"value"),
//!         fragment_root: true,
//!     },
//! )?;
//! let image = written.page().to_image();
//! let block = writer.append(page, BlockContent::Base, Compression::Lz4, &image)?;
//! table.push(page, Link::base(version(1), block))?;
//!
//! // Commit 2 changed one hash of it: a run with one record.
//! let mut run = Vec::new();
//! let mut records = DeltaRunWriter::new(page, None, &mut run);
//! records.push(&Delta::Hash {
//!     slot,
//!     field: HashField::Value,
//!     hash: &[9; 32],
//! })?;
//! records.finish()?;
//! let block = writer.append(page, BlockContent::Delta, Compression::Raw, &run)?;
//! table.push(page, Link::delta(version(2), block))?;
//!
//! let pool = BufferPool::new(PoolConfig {
//!     page_size: size,
//!     capacity: 1 << 20,
//! })?;
//! let loader = PageLoader::new(&pool, &table, &segments, size)?;
//! let mut buffers = LoadBuffers::new();
//!
//! // A reader at version 2 sees the changed hash, a reader at 1 the old one.
//! let new = loader.get(page, version(2), &mut buffers)?;
//! let cell = TreePage::from_validated(&new, FRAME_FORMAT)?.cell(slot).expect("the cell");
//! assert_eq!((new.born(), cell.value_hash()), (version(2), &[9; 32]));
//! let old = loader.get(page, version(1), &mut buffers)?;
//! let cell = TreePage::from_validated(&old, FRAME_FORMAT)?.cell(slot).expect("the cell");
//! assert_eq!((old.born(), cell.value_hash()), (version(1), &[1; 32]));
//!
//! // The second time the page is in the pool, and nothing is read.
//! let again = loader.get(page, version(2), &mut buffers)?;
//! assert_eq!(pool.stats().hits, 1);
//! # drop((new, old, again));
//! # Ok(())
//! # }
//! ```
//!
//! # What a load does
//!
//! 1. [`BufferPool::get`]. A hit is the answer, and nothing else happens.
//! 2. After the miss, the page version is looked up in the page table, in
//!    one step ([`PageTable::read_version`], T5): the blocks of its base
//!    image and of the runs after it, the version it was born at, and the
//!    version of the link that follows it, if there is one. A page that has
//!    no version there is [`LoadError::NoPage`], and no frame is taken for
//!    it.
//!    Where the chain has more than one block, the file layer is told of
//!    all of them before the first is read ([`SegmentStore::will_read`]):
//!    they were written at different times and so, as a rule, lie apart,
//!    and a file layer that acts on the hint can bring them in at the
//!    same time. Nothing of the load depends on it.
//! 3. The base image's block is read ([`SegmentStore::read`], which verifies
//!    its checksum) and its payload is written to the start of a frame. The
//!    rest of the frame is set to zero. The image is checked in full
//!    (`TreePage::decode` or `FlatPage::decode`, by its first byte), and it
//!    must be the image of the page asked for and all the block holds.
//! 4. Each run's block is read and checked (`DeltaRun::decode`) and applied
//!    to the frame (`apply_chain`), oldest first.
//! 5. [`BufferPool::install_loaded`], with the two versions of step 2. If it
//!    answers [`Loaded::Again`], the pool is asked again, and if the page is
//!    still not there the load is made again from step 2, into the same
//!    frame. So a load is repeated for as long as commits and page steps
//!    keep overtaking it in its part of the pool's table.
//!
//! On any error the frame goes back to the pool and nothing is installed. A
//! run that was applied in part is never seen (`page-delta.md` A3).
//!
//! # What is checked
//!
//! | Check | By | Error |
//! | --- | --- | --- |
//! | The bytes of a block are the bytes that were written: its checksum, which covers its tag and its body. | [`SegmentStore::read`] | [`LoadError::Block`] |
//! | The block is of the kind the chain says: a base image first, runs after it. | the block's tag | [`LoadError::Content`] |
//! | The payload decompresses to exactly the length it states. A compressed block that states a length no longer than its stored bytes is not a block, and the segment store refuses it. A run that states more than 255 times the length of its block, which no LZ4 block reaches, is refused before memory is taken for it. A wrong length between the two is found by the decompression, into memory of the stated length. | [`SegmentStore::read`], `Block::copy_payload_to` | [`LoadError::Block`], [`LoadError::Payload`] |
//! | The image is a well-formed page of the store's page size. | `TreePage::decode`, `FlatPage::decode` | [`LoadError::Payload`] |
//! | The block holds the image and nothing behind it (`segment-manifest.md` S7). | the image's own length | [`LoadError::TrailingBytes`] |
//! | The image is of the page asked for (`tree-page.md` TP2, `flat-page.md` P14). | the page id in its header | [`LoadError::Payload`] with `DeltaError::WrongPage` |
//! | A run is well formed and of the page asked for (`page-delta.md` PD3, A2). | `DeltaRun::decode`, `apply_chain` | [`LoadError::Payload`] |
//! | Every record of a run applies to the page beneath it (A3). | `apply_chain` | [`LoadError::Payload`] |
//! | A run is no longer than the store allows: its payload is at most one page size ([`max_run_len`]), which is the longest run write-back writes. A block that is longer than such a run can be stored as is refused before it is read, and a run that states a longer payload before memory is taken for it. | the block's location, the payload's stated length | [`LoadError::RunTooLong`] |
//! | A run in a segment does not create its page (PD11), and a flat page has no run (PD13). | the run, the image's kind | [`LoadError::CreatedInSegment`], [`LoadError::FlatPageDelta`] |
//! | No value is pending in a base image, or in the page as the last record of a run leaves it (PD16, P15). A value may be pending between two records of one run. | `has_pending_value` | [`LoadError::PendingValue`] |
//!
//! **A block with any single changed byte is refused.** The checksum is
//! CRC-32C over the block's tag and body, which detects every change that
//! lies within 32 consecutive bits. A changed byte of the length in the tag
//! makes the block longer than its location, or shorter, and then the
//! checksum is of other bytes and the location holds more than the block.
//! The other checks are for what a checksum cannot see: a block that is
//! whole and is the wrong one.
//!
//! # What a frame holds
//!
//! A loaded frame holds the page at its start, in the format version
//! [`FRAME_FORMAT`], and bytes that mean nothing behind it. A tree page is a
//! page in a buffer: `TreePage::from_validated` reads it, and a private
//! copy of the frame is opened with `TreePageMut::open`. After runs were
//! applied it may have unused bytes among its cells, so it is a page and
//! need not be an image. A flat page is its image, for
//! `FlatPage::from_validated`. Neither holds a pending value: a page with
//! one is in the pool because a commit installed it, never because it was
//! loaded.
//!
//! A block is read under the format version of its segment's file
//! ([`SegmentStore::format_version`]). With one format version a page needs
//! no conversion on its way into a frame; the task that adds a version says
//! how an older page gets there (`format.md` U9).
//!
//! # What the callers do
//!
//! - **A reader holds an operation guard** for the whole load
//!   (`transactions.md` PN5): it takes the guard before it reads the latest
//!   version, or before it asks at a version it has pinned, and holds it
//!   until the load has returned. It asks at a version it has pinned or at
//!   the latest version it read under the guard, as it asks the pool. A pin
//!   keeps the page version; the guard is what keeps the blocks that the
//!   page table named readable while they are read, when a cleaner moves
//!   them meanwhile (PN6). Without it a load can fail with
//!   [`LoadError::Block`] for a segment that is gone, or with
//!   [`LoadError::NoPage`] for a version that was trimmed.
//! - **It holds what it loaded** until it has used it. A page that was
//!   loaded into a frame beside the pool's fixed ones is gone when its last
//!   [`PageRef`] is dropped.
//! - **Whoever changes the page table** changes it before the pool is told,
//!   which is the pool's rule. A load reads the table once, after its miss,
//!   and the pool refuses what a commit, a free or a page step overtook.
//! - **Whoever opens the store** gives the loader the page size of the
//!   manifest, which is the pool's frame size.
//!
//! A page version that only the log and the pool hold has no link
//! (`page-table.md` T6), and the pool does not evict it. So a miss means
//! that a segment holds the page version, and [`LoadError::NoPage`] that
//! the page asked for is not a page at that version.
//!
//! [`BufferPool::get`]: crate::cache::BufferPool::get
//! [`BufferPool::install_loaded`]: crate::cache::BufferPool::install_loaded
//! [`Loaded::Again`]: crate::cache::Loaded::Again
//! [`PageRef`]: crate::cache::PageRef
//! [`PageTable::read_version`]: crate::table::PageTable::read_version
//! [`SegmentStore::read`]: crate::segment::SegmentStore::read
//! [`SegmentStore::will_read`]: crate::segment::SegmentStore::will_read
//! [`SegmentStore::format_version`]: crate::segment::SegmentStore::format_version

mod error;

#[cfg(test)]
mod tests;

use lignin_format::{
    delta::{apply_chain, DeltaError, DeltaRun},
    flat_page::{FlatPage, FlatPageKind},
    segment::{Block, BlockContent, SegmentError, FRAME_LEN, MAX_BODY_LEN},
    tree_page::TreePage,
    CommitVersion, FormatVersion, PageId, PageSize,
};

pub use self::error::LoadError;
use crate::{
    cache::{BufferPool, Loaded, Miss, PageRef},
    flush::max_run_len,
    segment::{BlockLocation, SegmentStore},
    table::PageTable,
};

/// The format version of a page in a frame.
pub const FRAME_FORMAT: FormatVersion = FormatVersion::CURRENT;

/// The version a page is in once the image of a file of version `file` has
/// been written to a frame.
const fn in_frame(file: FormatVersion) -> FormatVersion {
    match file {
        FormatVersion::V1 => FRAME_FORMAT,
    }
}

/// The most bytes one stored byte of an LZ4 block decompresses to.
const LZ4_MAX_EXPANSION: usize = 255;

/// The memory a load reads into: the blocks of a chain, one block, and the
/// payload of a run.
///
/// It grows to what the largest load needed and is reused by the next, so a
/// reader keeps one for as long as it reads. With buffers that have grown,
/// a load allocates nothing of its own. What applying a record allocates
/// is the page codec's to say (`page-delta.md` A1): nothing for a hash.
#[derive(Debug, Default)]
pub struct LoadBuffers {
    chain: Vec<BlockLocation>,
    block: Vec<u8>,
    run: Vec<u8>,
}

impl LoadBuffers {
    /// Buffers that hold nothing yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// What a frame holds once a base image is in it.
#[derive(Debug, Clone, Copy)]
enum Filled {
    /// A flat page, which takes no run.
    Flat,
    /// A tree page in this format version.
    Tree(FormatVersion),
}

/// The read path of a store: its buffer pool, and behind it the page table
/// and the segments. See the [module documentation](self).
///
/// It is three references and a size, and can be copied to every reader.
#[derive(Debug, Clone, Copy)]
pub struct PageLoader<'s> {
    pool: &'s BufferPool,
    table: &'s PageTable,
    segments: &'s SegmentStore,
    size: PageSize,
}

impl<'s> PageLoader<'s> {
    /// A loader for the pages of one store: `table` says where they are in
    /// `segments`, and `pool` holds them in memory. `page_size` is the
    /// store's, from its manifest, and must be the size of the pool's
    /// frames ([`LoadError::FrameSize`]).
    pub fn new(
        pool: &'s BufferPool,
        table: &'s PageTable,
        segments: &'s SegmentStore,
        page_size: PageSize,
    ) -> Result<Self, LoadError> {
        if pool.frame_len() != page_size.get() {
            return Err(LoadError::FrameSize {
                page_size: page_size.get(),
                frame_len: pool.frame_len(),
            });
        }
        Ok(Self {
            pool,
            table,
            segments,
            size: page_size,
        })
    }

    /// The version of `page` that a reader at version `at` sees, pinned:
    /// from the pool if it is there, and otherwise loaded from its
    /// segments.
    ///
    /// A hit is [`BufferPool::get`] and nothing more. See [`Self::load`]
    /// for a miss.
    pub fn get(
        &self,
        page: PageId,
        at: CommitVersion,
        buffers: &mut LoadBuffers,
    ) -> Result<PageRef<'s>, LoadError> {
        self.pool
            .get(page, at)
            .or_else(|miss| self.load(miss, buffers))
    }

    /// Loads the page version that `miss` was for, and returns it pinned:
    /// for a caller that asked [`BufferPool::get`] itself.
    ///
    /// The page's blocks are read and checked as the
    /// [module documentation](self) says, into a frame of the pool. If
    /// another thread loaded the same page version meanwhile, its frame is
    /// returned. If a commit or a page step overtook the load, the pool is
    /// asked again, and the page is loaded again if it is still not there.
    ///
    /// On an error nothing was put into the pool.
    pub fn load(
        &self,
        mut miss: Miss,
        buffers: &mut LoadBuffers,
    ) -> Result<PageRef<'s>, LoadError> {
        let (page, at) = (miss.page(), miss.at());
        let mut taken = None;
        loop {
            let visible = self
                .table
                .read_version(page, at, &mut buffers.chain)
                .map_err(|freed| LoadError::NoPage { page, at, freed })?;
            let mut blank = taken.take().unwrap_or_else(|| self.pool.blank());
            self.fill(&mut blank, page, buffers)?;
            match self
                .pool
                .install_loaded(blank, miss, visible.born, visible.next)?
            {
                Loaded::Page(loaded) => return Ok(loaded),
                Loaded::Again(blank) => taken = Some(blank),
            }
            miss = match self.pool.get(page, at) {
                Ok(found) => return Ok(found),
                Err(miss) => miss,
            };
        }
    }

    /// Reads the block at `location`, which the chain of `page` says holds
    /// `expected`, and returns it with the format version of its segment.
    fn block<'b>(
        &self,
        page: PageId,
        location: BlockLocation,
        expected: BlockContent,
        max_payload_len: usize,
        buf: &'b mut Vec<u8>,
    ) -> Result<(Block<'b>, FormatVersion), LoadError> {
        let unread = |source| LoadError::Block {
            page,
            location,
            source,
        };
        let version = self
            .segments
            .format_version(location.file)
            .map_err(unread)?;
        let block = self
            .segments
            .read(location, max_payload_len, buf)
            .map_err(unread)?;
        if block.content() != expected {
            return Err(LoadError::Content {
                page,
                location,
                expected,
                found: block.content(),
            });
        }
        Ok((block, version))
    }

    /// Builds `page` in `frame` from the blocks of `buffers.chain`: its
    /// base image first, and the runs to apply to it after it.
    fn fill(
        &self,
        frame: &mut [u8],
        page: PageId,
        buffers: &mut LoadBuffers,
    ) -> Result<(), LoadError> {
        let LoadBuffers { chain, block, run } = buffers;
        // As a rule the blocks of a chain lie apart: the file layer is told
        // of all of them before the first is read.
        self.segments.will_read(chain);
        let mut filled = None;
        for &location in chain.iter() {
            filled = Some(match filled {
                None => self.image(frame, page, location, block)?,
                Some(Filled::Flat) => return Err(LoadError::FlatPageDelta { page, location }),
                Some(Filled::Tree(version)) => {
                    self.run(frame, page, location, version, block, run)?;
                    Filled::Tree(version)
                }
            });
        }
        Ok(())
    }

    /// Puts the base image in the block at `location` into `frame`, with
    /// zeros behind it.
    fn image(
        &self,
        frame: &mut [u8],
        page: PageId,
        location: BlockLocation,
        block: &mut Vec<u8>,
    ) -> Result<Filled, LoadError> {
        let refused = |source| LoadError::Payload {
            page,
            location,
            source,
        };
        let (stored, version) = self.block(
            page,
            location,
            BlockContent::Base,
            self.size.image_capacity(),
            block,
        )?;
        let (image, rest) = frame.split_at_mut(stored.payload_len());
        stored.copy_payload_to(image).map_err(refused)?;
        rest.fill(0);
        let is_flat = image
            .first()
            .is_some_and(|&code| FlatPageKind::from_code(code, version).is_ok());
        let (image_len, found, pending) = if is_flat {
            let flat = FlatPage::decode(image, version).map_err(refused)?;
            (flat.as_bytes().len(), flat.id(), flat.has_pending_value())
        } else {
            let tree = TreePage::decode(image, version, self.size).map_err(refused)?;
            (tree.encoded_len(), tree.id(), tree.has_pending_value())
        };
        if image_len != image.len() {
            return Err(LoadError::TrailingBytes {
                page,
                location,
                image_len,
                payload_len: image.len(),
            });
        }
        if found != page {
            return Err(refused(
                DeltaError::WrongPage {
                    found,
                    expected: page,
                }
                .into(),
            ));
        }
        if pending {
            return Err(LoadError::PendingValue { page, location });
        }
        Ok(if is_flat {
            Filled::Flat
        } else {
            Filled::Tree(in_frame(version))
        })
    }

    /// Applies the run in the block at `location` to the tree page in
    /// `frame`, which is in the format `version`.
    fn run(
        &self,
        frame: &mut [u8],
        page: PageId,
        location: BlockLocation,
        version: FormatVersion,
        block: &mut Vec<u8>,
        run: &mut Vec<u8>,
    ) -> Result<(), LoadError> {
        let refused = |source| LoadError::Payload {
            page,
            location,
            source,
        };
        // A run is stored raw or compressed to fewer bytes, so a block
        // whose body is longer than the longest run holds none, and is not
        // read.
        let max = max_run_len(self.size);
        let too_long = |len| LoadError::RunTooLong {
            page,
            location,
            len,
            max,
        };
        let stored_len = location.len as usize;
        if stored_len > FRAME_LEN + max {
            return Err(too_long(stored_len - FRAME_LEN));
        }
        let (stored, run_version) =
            self.block(page, location, BlockContent::Delta, MAX_BODY_LEN, block)?;
        if stored.payload_len() > stored_len.saturating_mul(LZ4_MAX_EXPANSION) {
            return Err(refused(SegmentError::Decompression.into()));
        }
        if stored.payload_len() > max {
            return Err(too_long(stored.payload_len()));
        }
        run.clear();
        run.resize(stored.payload_len(), 0);
        stored.copy_payload_to(run).map_err(refused)?;
        let changes = DeltaRun::decode(run, run_version).map_err(refused)?;
        if changes.page() == page && changes.created().is_some() {
            return Err(LoadError::CreatedInSegment { page, location });
        }
        apply_chain(page, frame, self.size, version, [&changes]).map_err(refused)?;
        let after = TreePage::from_validated(frame, version).map_err(refused)?;
        if after.has_pending_value() {
            return Err(LoadError::PendingValue { page, location });
        }
        Ok(())
    }
}
