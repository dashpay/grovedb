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
//! runs to it in order, and hands the frame to the pool. Nothing in a page
//! is computed: every hash in the frame is the hash a block held.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{
//!     delta::{Delta, DeltaRunWriter},
//!     manifest::Manifest,
//!     segment::{BlockContent, Compression},
//!     tree_page::{CellContent, CellValue, HashField, TreePage, TreePageMut, TreePageSize},
//!     CommitVersion, PageId, SlotId,
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
//! let size = TreePageSize::DEFAULT;
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
//! 2. After the miss, the page's chain is read from the page table in one
//!    step ([`PageTable::chain`]). The newest link at or before the version
//!    asked for, and the links back to the nearest base image, are the page
//!    version (T5); its born version is that newest link's, and the version
//!    of the link after it, if there is one, is where it ends.
//! 3. The base image's block is read ([`SegmentStore::read`], which verifies
//!    its checksum) and its payload is written to the start of a frame. The
//!    rest of the frame is set to zero. The image is checked in full
//!    (`TreePage::decode` or `FlatPage::decode`, by its first byte), and it
//!    must be the image of the page asked for and all the block holds.
//! 4. Each run's block is read and checked (`DeltaRun::decode`) and applied
//!    to the frame (`apply_chain`), oldest first.
//! 5. [`BufferPool::install_loaded`], with the two versions of step 2. If it
//!    answers [`Loaded::Again`], the load starts again at step 1 with the
//!    same frame.
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
//! | The payload decompresses to the length it states. | `Block::copy_payload_to` | [`LoadError::Payload`] |
//! | The image is a well-formed page of the store's page size. | `TreePage::decode`, `FlatPage::decode` | [`LoadError::Payload`] |
//! | The block holds the image and nothing behind it (`segment-manifest.md` S7). | the image's own length | [`LoadError::TrailingBytes`] |
//! | The image is of the page asked for (`tree-page.md` TP2). | the page id in its header | [`LoadError::Payload`] with `DeltaError::WrongPage` |
//! | A run is well formed and of the page asked for (`page-delta.md` PD3, A2). | `DeltaRun::decode`, `apply_chain` | [`LoadError::Payload`] |
//! | Every record of a run applies to the page beneath it (A3). | `apply_chain` | [`LoadError::Payload`] |
//! | A run in a segment does not create its page (PD11), and a flat page has no run (PD13). | the run, the image's kind | [`LoadError::CreatedInSegment`], [`LoadError::FlatPageDelta`] |
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
//! `FlatPage::from_validated`.
//!
//! A block is read under the format version of its segment's file
//! ([`SegmentStore::format_version`]). With one format version a page needs
//! no conversion on its way into a frame; the task that adds a version says
//! how an older page gets there (`format.md` U9).
//!
//! # What the callers do
//!
//! - **A reader** asks at a version it has pinned, or at the latest version
//!   under an operation guard (`transactions.md` PN5), as it asks the pool.
//!   That is also what keeps the blocks of the chain it is given readable
//!   while it reads them (PN6).
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
//! [`PageTable::chain`]: crate::table::PageTable::chain
//! [`SegmentStore::read`]: crate::segment::SegmentStore::read
//! [`SegmentStore::format_version`]: crate::segment::SegmentStore::format_version

mod error;

#[cfg(test)]
mod tests;

use std::ops::Range;

use lignin_format::{
    delta::{apply_chain, DeltaError, DeltaRun},
    flat_page::{FlatPage, FlatPageKind},
    segment::{Block, BlockContent, MAX_BODY_LEN},
    tree_page::{TreePage, TreePageSize},
    CommitVersion, FormatVersion, PageId,
};

pub use self::error::LoadError;
use crate::{
    cache::{BufferPool, Loaded, Miss, PageRef},
    segment::{BlockLocation, SegmentStore},
    table::{Link, LinkKind, PageTable},
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

/// The memory a load reads into: the links of a chain, one block, and the
/// payload of a run.
///
/// It grows to what the largest load needed and is reused by the next, so a
/// reader keeps one for as long as it reads. With buffers that have grown,
/// a load allocates nothing of its own. What applying a record allocates
/// is the page codec's to say (`page-delta.md` A1): nothing for a hash.
#[derive(Debug, Default)]
pub struct LoadBuffers {
    links: Vec<Link>,
    block: Vec<u8>,
    run: Vec<u8>,
}

impl LoadBuffers {
    /// Buffers that hold nothing yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The part of a chain that is one page version (`page-table.md` T5).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selected {
    base: BlockLocation,
    /// The places in the chain of the runs to apply to the base image.
    runs: Range<usize>,
    born: CommitVersion,
    next: Option<CommitVersion>,
}

impl Selected {
    /// What a reader of `page` at `at` is given of `links`, the page's
    /// chain from its oldest link.
    fn of(page: PageId, at: CommitVersion, links: &[Link]) -> Result<Self, LoadError> {
        let no_page = |freed| LoadError::NoPage { page, at, freed };
        let newest = links
            .iter()
            .rposition(|link| link.version <= at)
            .ok_or(no_page(None))?;
        let born = links[newest].version;
        if links[newest].kind == LinkKind::Free {
            return Err(no_page(Some(born)));
        }
        let below_runs = links[..=newest]
            .iter()
            .rposition(|link| !matches!(link.kind, LinkKind::Delta(_)));
        let Some((base_at, LinkKind::Base(base))) = below_runs.map(|at| (at, links[at].kind))
        else {
            return Err(LoadError::NoBase { page, born });
        };
        Ok(Self {
            base,
            runs: base_at + 1..newest + 1,
            born,
            next: links.get(newest + 1).map(|link| link.version),
        })
    }
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
    size: TreePageSize,
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
        page_size: TreePageSize,
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
        match self.pool.get(page, at) {
            Ok(found) => Ok(found),
            Err(miss) => self.load(miss, buffers),
        }
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
        let mut blank = self.pool.blank();
        loop {
            let filled = self.fill(&mut blank, miss.page(), miss.at(), buffers)?;
            blank = match self
                .pool
                .install_loaded(blank, miss, filled.born, filled.next)?
            {
                Loaded::Page(loaded) => return Ok(loaded),
                Loaded::Again(blank) => blank,
            };
            miss = match self.pool.get(miss.page(), miss.at()) {
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

    /// Builds in `frame` the version of `page` that a reader at `at` sees,
    /// from the page table as it is now.
    fn fill(
        &self,
        frame: &mut [u8],
        page: PageId,
        at: CommitVersion,
        buffers: &mut LoadBuffers,
    ) -> Result<Selected, LoadError> {
        let LoadBuffers { links, block, run } = buffers;
        links.clear();
        self.table.chain(page, links);
        let selected = Selected::of(page, at, links)?;
        let base = selected.base;

        let payload = |location| {
            move |source| LoadError::Payload {
                page,
                location,
                source,
            }
        };
        let (image, version) = self.block(
            page,
            base,
            BlockContent::Base,
            self.size.image_capacity(),
            block,
        )?;
        let (start, rest) = frame.split_at_mut(image.payload_len());
        image.copy_payload_to(start).map_err(payload(base))?;
        rest.fill(0);
        let is_flat = start
            .first()
            .is_some_and(|&code| FlatPageKind::from_code(code, version).is_ok());
        let (image_len, found) = if is_flat {
            let flat = FlatPage::decode(start, version).map_err(payload(base))?;
            (flat.as_bytes().len(), page)
        } else {
            let tree = TreePage::decode(start, version, self.size).map_err(payload(base))?;
            (tree.encoded_len(), tree.id())
        };
        if image_len != start.len() {
            return Err(LoadError::TrailingBytes {
                page,
                location: base,
                image_len,
                payload_len: start.len(),
            });
        }
        if found != page {
            let wrong = DeltaError::WrongPage {
                found,
                expected: page,
            };
            return Err(payload(base)(wrong.into()));
        }

        let version = in_frame(version);
        for location in links[selected.runs.clone()]
            .iter()
            .filter_map(Link::location)
        {
            if is_flat {
                return Err(LoadError::FlatPageDelta { page, location });
            }
            let (stored, run_version) =
                self.block(page, location, BlockContent::Delta, MAX_BODY_LEN, block)?;
            run.clear();
            run.resize(stored.payload_len(), 0);
            stored.copy_payload_to(run).map_err(payload(location))?;
            let changes = DeltaRun::decode(run, run_version).map_err(payload(location))?;
            if changes.created().is_some() {
                return Err(LoadError::CreatedInSegment { page, location });
            }
            apply_chain(page, frame, self.size, version, [&changes]).map_err(payload(location))?;
        }
        Ok(selected)
    }
}
