//! The error a load returns.

use std::fmt;

use lignin_format::{segment::BlockContent, CommitVersion, FormatError, PageId};

use crate::{
    cache::CacheError,
    segment::{BlockLocation, SegmentStoreError},
};

/// Why a page was not loaded. Nothing was put into the pool.
///
/// [`Self::NoPage`] says that the page table has no such page version, and
/// [`Self::FrameSize`] that the loader was put together wrongly. A
/// [`Self::Block`] that holds an error of the file layer is the file
/// layer's failure. Every other error says that what the segments hold is
/// not what the page table says they hold: damage, or a bug of whoever wrote
/// it.
#[derive(Debug)]
pub enum LoadError {
    /// The pool's frames are not of the store's page size.
    FrameSize {
        /// The store's page size in bytes.
        page_size: usize,
        /// The bytes of one frame of the pool.
        frame_len: usize,
    },
    /// No segment holds a version of the page that a reader at this version
    /// sees: the page has no link that old, or it was freed by then.
    NoPage {
        /// The page asked for.
        page: PageId,
        /// The version it was asked at.
        at: CommitVersion,
        /// The version of the commit that freed the page, if one did.
        freed: Option<CommitVersion>,
    },
    /// The page's chain has delta runs with no base image beneath them.
    NoBase {
        /// The page.
        page: PageId,
        /// The version of the newest link a reader at the version asked for
        /// is given.
        born: CommitVersion,
    },
    /// A block of the page's chain could not be read: the file layer failed,
    /// the location is not a block of a segment the store has, or the bytes
    /// there are not a block whose checksum matches.
    Block {
        /// The page.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What the segment store reported.
        source: SegmentStoreError,
    },
    /// A block of the page's chain holds the other kind of payload: a run
    /// of deltas where the chain has a base image, or a base image where it
    /// has a run.
    Content {
        /// The page.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What the chain says the block holds.
        expected: BlockContent,
        /// What the block says it holds.
        found: BlockContent,
    },
    /// The payload of a block is not what it has to be: it does not
    /// decompress, it is not a page image or not a run of deltas, it is of
    /// another page, or a run does not apply to the page beneath it.
    Payload {
        /// The page.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What is wrong with it.
        source: FormatError,
    },
    /// A block holds bytes behind the end of its base image.
    TrailingBytes {
        /// The page.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// The length the image says it has.
        image_len: usize,
        /// The length of the block's payload.
        payload_len: usize,
    },
    /// The chain of a flat page has a run of deltas. A flat page takes none.
    FlatPageDelta {
        /// The page.
        page: PageId,
        /// The block of the run.
        location: BlockLocation,
    },
    /// A run in a segment creates its page. Only a run in the log does: a
    /// page's chain in the segments starts with a base image.
    CreatedInSegment {
        /// The page.
        page: PageId,
        /// The block of the run.
        location: BlockLocation,
    },
    /// The pool refused the page: what the page table says of it cannot be
    /// true of the page versions the pool holds.
    Cache(CacheError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let block = |location: &BlockLocation| {
            format!(
                "the block at offset {} of segment {}",
                location.offset, location.file
            )
        };
        match self {
            Self::FrameSize {
                page_size,
                frame_len,
            } => write!(
                f,
                "the store's pages are {page_size} bytes and the pool's frames {frame_len}"
            ),
            Self::NoPage {
                page,
                at,
                freed: Some(freed),
            } => write!(
                f,
                "page {page} has no version at {at}: the commit of version {freed} freed it"
            ),
            Self::NoPage {
                page,
                at,
                freed: None,
            } => write!(f, "no segment holds a version of page {page} at {at}"),
            Self::NoBase { page, born } => write!(
                f,
                "the chain of page {page} up to version {born} has no base image"
            ),
            Self::Block {
                page,
                location,
                source,
            } => write!(
                f,
                "{} could not be read for page {page}: {source}",
                block(location)
            ),
            Self::Content {
                page,
                location,
                expected,
                found,
            } => write!(
                f,
                "{} holds a {} where the chain of page {page} has a {}",
                block(location),
                content(*found),
                content(*expected)
            ),
            Self::Payload {
                page,
                location,
                source,
            } => write!(f, "{} does not give page {page}: {source}", block(location)),
            Self::TrailingBytes {
                page,
                location,
                image_len,
                payload_len,
            } => write!(
                f,
                "{} holds {payload_len} bytes for page {page}, whose image is {image_len} long",
                block(location)
            ),
            Self::FlatPageDelta { page, location } => write!(
                f,
                "{} is a run of deltas for page {page}, which is a flat page",
                block(location)
            ),
            Self::CreatedInSegment { page, location } => write!(
                f,
                "{} is a run that creates page {page}, in a segment",
                block(location)
            ),
            Self::Cache(source) => write!(f, "the buffer pool refused the page: {source}"),
        }
    }
}

fn content(content: BlockContent) -> &'static str {
    match content {
        BlockContent::Base => "base image",
        BlockContent::Delta => "run of deltas",
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Block { source, .. } => Some(source),
            Self::Payload { source, .. } => Some(source),
            Self::Cache(source) => Some(source),
            _ => None,
        }
    }
}

impl From<CacheError> for LoadError {
    fn from(source: CacheError) -> Self {
        Self::Cache(source)
    }
}
