//! The errors the cleaner returns.

use std::fmt;

use lignin_format::{FileId, FormatError, PageId};

use crate::{
    flush::FlushError,
    segment::{BlockLocation, SegmentStoreError},
    table::LinkError,
    values::ValueStoreError,
};

/// Why a pass of the cleaner failed.
///
/// Nothing is lost by any of these. A block that was not moved is where it
/// was, with its link; a copy that no link points to is dead space; and a
/// file that was not deleted is deleted by a later pass or by the next
/// session. A pass that failed can be made again.
#[derive(Debug)]
pub enum CleanerError {
    /// The segment store or the manifest failed.
    Segments(SegmentStoreError),
    /// The value log failed.
    Values(ValueStoreError),
    /// Write-back failed: a table checkpoint the cleaner asked for, the
    /// sealing of the segment that was being filled, the sync of the log,
    /// or the manifest.
    Flush(FlushError),
    /// A block that a link points to could not be read. Its segment is
    /// left as it is: no further block of it is moved in this session.
    Block {
        /// The page whose chain has the link.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What the segment store reported.
        source: SegmentStoreError,
    },
    /// The payload of a block that was read is not what its tag says.
    Payload {
        /// The page whose chain has the link.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What the format reported.
        source: FormatError,
    },
    /// The page table refused to point a link at the copy of its block,
    /// for a reason other than the link being gone.
    Link {
        /// The page.
        page: PageId,
        /// The block the link pointed to.
        from: BlockLocation,
        /// The copy.
        to: BlockLocation,
        /// What the table reported.
        source: LinkError,
    },
    /// A sync of the segment that takes the moved blocks failed. The blocks
    /// in it that were not durable stay readable until the session ends,
    /// and no table checkpoint is installed in it any more.
    SegmentNotDurable {
        /// The segment.
        file: FileId,
        /// What the segment store reported.
        source: SegmentStoreError,
    },
}

impl CleanerError {
    pub(super) fn block(
        page: PageId,
        location: BlockLocation,
    ) -> impl FnOnce(SegmentStoreError) -> Self {
        move |source| Self::Block {
            page,
            location,
            source,
        }
    }

    pub(super) fn payload(
        page: PageId,
        location: BlockLocation,
    ) -> impl FnOnce(FormatError) -> Self {
        move |source| Self::Payload {
            page,
            location,
            source,
        }
    }
}

impl fmt::Display for CleanerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Segments(source) => write!(f, "the cleaner's segments: {source}"),
            Self::Values(source) => write!(f, "the cleaner's values: {source}"),
            Self::Flush(source) => write!(f, "write-back, for the cleaner: {source}"),
            Self::Block {
                page,
                location,
                source,
            } => write!(
                f,
                "the block of page {page} at offset {} of segment {} could not be read: {source}",
                location.offset, location.file
            ),
            Self::Payload {
                page,
                location,
                source,
            } => write!(
                f,
                "the block of page {page} at offset {} of segment {} is not what its tag says: \
                 {source}",
                location.offset, location.file
            ),
            Self::Link {
                page,
                from,
                to,
                source,
            } => write!(
                f,
                "the link of page {page} to offset {} of segment {} was not pointed at offset {} \
                 of segment {}: {source}",
                from.offset, from.file, to.offset, to.file
            ),
            Self::SegmentNotDurable { file, source } => write!(
                f,
                "segment {file}, which takes the moved blocks, could not be made durable: {source}"
            ),
        }
    }
}

impl std::error::Error for CleanerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segments(source)
            | Self::Block { source, .. }
            | Self::SegmentNotDurable { source, .. } => Some(source),
            Self::Values(source) => Some(source),
            Self::Flush(source) => Some(source),
            Self::Payload { source, .. } => Some(source),
            Self::Link { source, .. } => Some(source),
        }
    }
}

impl From<SegmentStoreError> for CleanerError {
    fn from(source: SegmentStoreError) -> Self {
        Self::Segments(source)
    }
}

impl From<ValueStoreError> for CleanerError {
    fn from(source: ValueStoreError) -> Self {
        Self::Values(source)
    }
}

impl From<FlushError> for CleanerError {
    fn from(source: FlushError) -> Self {
        Self::Flush(source)
    }
}
