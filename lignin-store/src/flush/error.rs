//! The errors write-back returns.

use std::fmt;

use lignin_format::{CommitVersion, FormatError, PageId};

use crate::{
    cache::{CacheError, StepRefusal},
    segment::{BlockLocation, SegmentStoreError},
    table::{LinkError, LogPlace, PageTableError},
    values::ValueStoreError,
    wal::LogError,
};

/// Why a call of write-back failed.
///
/// Nothing that was committed is lost by any of these: a page version that
/// was not written back is still in the buffer pool and in the log, and a
/// table checkpoint that was not installed leaves the one before it in
/// force. A call that failed can be made again.
#[derive(Debug)]
pub enum FlushError {
    /// The pool's frames are not of the store's page size.
    FrameSize {
        /// The store's page size in bytes.
        page_size: usize,
        /// The bytes of one frame of the pool.
        frame_len: usize,
    },
    /// The segment store or the manifest failed.
    Segments(SegmentStoreError),
    /// The page table refused a checkpoint, or its file layer failed.
    Table(PageTableError),
    /// The value log failed.
    Values(ValueStoreError),
    /// The log failed.
    Log(LogError),
    /// The buffer pool refused a call. Write-back broke one of the pool's
    /// rules, or was given a handle of another pool.
    Cache(CacheError),
    /// A page in a frame, or a block of its chain, is not what its format
    /// allows.
    Page {
        /// The page.
        page: PageId,
        /// What the format reported.
        source: FormatError,
    },
    /// The page table refused the link of a block that was written, for a
    /// reason other than a free link that a commit pushed meanwhile.
    Link {
        /// The page.
        page: PageId,
        /// The block.
        location: BlockLocation,
        /// What the table reported.
        source: LinkError,
    },
    /// A page step that had written its record was refused by the pool.
    /// The record is in the log, and the change was not made in memory.
    StepRefused {
        /// The page.
        page: PageId,
        /// What the pool reported.
        source: StepRefusal,
    },
    /// The bytes of a pending value are neither with the frame of its page
    /// nor of the length its cell states.
    PendingValue {
        /// The page.
        page: PageId,
        /// The version that bore the page version.
        born: CommitVersion,
    },
    /// The place of a sample of the value counts is before the place of
    /// the sample before it (`page-table.md` K15). Nothing was written.
    SampleWentBack {
        /// The place of the sample before.
        before: LogPlace,
        /// The place of the sample that was taken.
        taken: LogPlace,
    },
    /// A sync of a segment failed earlier in this session. The blocks that
    /// were not durable then cannot be made durable, so no table checkpoint
    /// is installed until the store is opened again. Reads and commits go
    /// on.
    SegmentsNotDurable,
}

impl fmt::Display for FlushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameSize {
                page_size,
                frame_len,
            } => write!(
                f,
                "the store's pages are {page_size} bytes and the pool's frames {frame_len}"
            ),
            Self::Segments(source) => write!(f, "write-back: {source}"),
            Self::Table(source) => write!(f, "write-back: {source}"),
            Self::Values(source) => write!(f, "write-back: {source}"),
            Self::Log(source) => write!(f, "write-back: {source}"),
            Self::Cache(source) => write!(f, "write-back: {source}"),
            Self::Page { page, source } => {
                write!(f, "page {page} cannot be written back: {source}")
            }
            Self::Link {
                page,
                location,
                source,
            } => write!(
                f,
                "the page table refused the block of page {page} at offset {} of segment {}: \
                 {source}",
                location.offset, location.file
            ),
            Self::StepRefused { page, source } => write!(
                f,
                "a page step on page {page} was refused after its record was written: {source}"
            ),
            Self::PendingValue { page, born } => write!(
                f,
                "a pending value of page {page} as born at version {born} has no bytes of its \
                 length"
            ),
            Self::SampleWentBack { before, taken } => write!(
                f,
                "a sample of the value counts was taken at offset {} of log file {}, before the \
                 sample at offset {} of log file {}",
                taken.offset(),
                taken.file(),
                before.offset(),
                before.file()
            ),
            Self::SegmentsNotDurable => write!(
                f,
                "a sync of a segment failed in this session, and no table checkpoint is \
                 installed until the store is opened again"
            ),
        }
    }
}

impl std::error::Error for FlushError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segments(source) => Some(source),
            Self::Table(source) => Some(source),
            Self::Values(source) => Some(source),
            Self::Log(source) => Some(source),
            Self::Cache(source) => Some(source),
            Self::Page { source, .. } => Some(source),
            Self::Link { source, .. } => Some(source),
            Self::StepRefused { source, .. } => Some(source),
            Self::FrameSize { .. }
            | Self::PendingValue { .. }
            | Self::SampleWentBack { .. }
            | Self::SegmentsNotDurable => None,
        }
    }
}

impl From<SegmentStoreError> for FlushError {
    fn from(source: SegmentStoreError) -> Self {
        Self::Segments(source)
    }
}

impl From<PageTableError> for FlushError {
    fn from(source: PageTableError) -> Self {
        Self::Table(source)
    }
}

impl From<ValueStoreError> for FlushError {
    fn from(source: ValueStoreError) -> Self {
        Self::Values(source)
    }
}

impl From<LogError> for FlushError {
    fn from(source: LogError) -> Self {
        Self::Log(source)
    }
}

impl From<CacheError> for FlushError {
    fn from(source: CacheError) -> Self {
        Self::Cache(source)
    }
}

impl FlushError {
    pub(super) fn page(page: PageId) -> impl FnOnce(FormatError) -> Self {
        move |source| Self::Page { page, source }
    }
}
