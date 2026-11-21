//! The error a recovery returns.

use std::fmt;

use lignin_format::{value_log::ValuePointer, wal::LogPlace, CommitVersion, FormatError, PageId};

use crate::{
    alloc::RecoveryError as AllocatorError,
    cache::{CacheError, StepRefusal},
    load::LoadError,
    segment::SegmentStoreError,
    table::{LinkError, PageTableError},
    values::{Miscount, ValueStoreError},
    wal::LogError,
};

/// Why a store was not recovered.
///
/// A recovery that fails has made nothing durable that the next one cannot
/// start from: the manifest names the table checkpoint it named, and the
/// log it names holds what it held. So a recovery that failed because the
/// file layer did can be made again.
///
/// Some of the variants say that what the directory holds is not what a
/// session of this store leaves, whatever the crash: the files are
/// damaged, or a rule that keeps the log ahead of the table checkpoint was
/// broken by whoever wrote them. Recovery repairs none of them.
/// [`Self::is_damage`] says which they are, and tells them from a failure
/// of the file layer and from the error of a part of the store, which says
/// for itself what it found.
#[derive(Debug)]
pub enum RecoveryError {
    /// The manifest could not be read or replaced, or a segment could not
    /// be opened, written or sealed.
    Segments(SegmentStoreError),
    /// The table checkpoint the manifest names could not be loaded.
    Table(PageTableError),
    /// The value log could not be opened, or its counts speak of a file
    /// that is gone.
    Values(ValueStoreError),
    /// The log could not be read to its end, opened there or turned to a
    /// file of this session, or a pending value could not be read from it.
    Log(LogError),
    /// The buffer pool refused a page that replay rebuilt.
    Pool(CacheError),
    /// The buffer pool refused the frame of a page that only background
    /// changes were replayed on.
    Step {
        /// The page.
        page: PageId,
        /// Why the frame was refused.
        source: StepRefusal,
    },
    /// The page id allocator could not be derived: a link of the table or
    /// a replayed change is of a later version than the one recovery
    /// landed on.
    Allocator(AllocatorError),
    /// The loader of the store's pages could not be made.
    Loader {
        /// What it reported.
        source: LoadError,
    },
    /// A page that a logged change is to be made to could not be read from
    /// its segments.
    Page {
        /// The page.
        page: PageId,
        /// Where the item with the change begins in the log.
        at: LogPlace,
        /// What the loader reported.
        source: LoadError,
    },
    /// A change that the log holds does not apply to the page it names, as
    /// the table checkpoint and the changes before it give that page.
    Change {
        /// The page.
        page: PageId,
        /// Where the item with the change begins in the log.
        at: LogPlace,
        /// What the page codec reported.
        source: FormatError,
    },
    /// A page that replay rebuilt is not a page of its format.
    Rebuilt {
        /// The page.
        page: PageId,
        /// What the page codec reported.
        source: FormatError,
    },
    /// The page table refused the link of a page that replay freed or
    /// wrote to a segment.
    Link {
        /// The page.
        page: PageId,
        /// Why the link was refused.
        source: LinkError,
    },
    /// The log ends before the commit that the table checkpoint covers. A
    /// checkpoint is named only once the log is durable through what it
    /// covers (`page-table.md` K4).
    LogBehindCheckpoint {
        /// The version the checkpoint covers.
        covers: CommitVersion,
        /// The version of the last commit group of the log.
        landed: CommitVersion,
    },
    /// The log does not hold the group of the commit after the one the
    /// table checkpoint covers: its first group, or its end if it holds no
    /// group, is of a later version. The log is kept from that group on
    /// (`page-table.md` K1).
    CommitsMissing {
        /// The version the checkpoint covers, or 0 with no checkpoint.
        covers: CommitVersion,
        /// The version the log goes on from.
        found: CommitVersion,
    },
    /// The table checkpoint holds links of a later version than the one
    /// recovery landed on. The log goes first (`page-table.md` K4), so
    /// that rule was broken, and recovery does not discard the links: what
    /// they replaced may be gone.
    LinksAfterLanding {
        /// How many such links the table held.
        links: u64,
        /// The version recovery landed on.
        landed: CommitVersion,
    },
    /// No checkpoint mark ends at the place of the sample that the table
    /// checkpoint carries (`log-format.md` LG29): the checkpoint or the
    /// log is not the one that was written.
    MarkMissing {
        /// The place of the sample.
        place: LogPlace,
    },
    /// The mark that ends at the place of the checkpoint's sample is the
    /// mark of a checkpoint that covers another version (LG29).
    WrongMark {
        /// The place of the sample.
        place: LogPlace,
        /// The version the mark says its checkpoint covers.
        mark: CommitVersion,
        /// The version the table checkpoint covers.
        covers: CommitVersion,
    },
    /// Replay met changes to pages that the table checkpoint has no image
    /// of, and no later commit freed those pages or made them again
    /// (`page-table.md` K11).
    ChangesWithoutPage {
        /// The lowest such page.
        page: PageId,
        /// How many such pages there are.
        pages: usize,
    },
    /// A replayed change makes a cell hold a pointer to a value that the
    /// value log does not have: its file is not there, or the manifest
    /// gives the file a valid length that ends before the value.
    ValueNotThere {
        /// The pointer.
        pointer: ValuePointer,
        /// Where the item with the change begins in the log.
        at: LogPlace,
        /// What the value log found.
        why: Miscount,
    },
    /// A reclaiming step frees a page that is in use and does not hold
    /// what the step took out of it. A page that has the step is freed,
    /// and one that lacks it holds those cells (`log-format.md` LG28).
    StepOnAnotherPage {
        /// The page.
        page: PageId,
        /// Where the record of the step begins in the log.
        at: LogPlace,
    },
    /// A pending value of a rebuilt page is named by the log, not carried
    /// in it (`log-format.md` LG23). No session of this version writes
    /// such a group, because nothing keeps the file a named value is in
    /// (LG24).
    NamedValue {
        /// The page whose cell holds the pending value.
        page: PageId,
    },
}

impl RecoveryError {
    /// Whether the error says that the directory does not hold what a
    /// session of this store leaves, however it ended: damage, or a broken
    /// rule of whoever wrote it. `false` for an error that may be the file
    /// layer's, and for a part of the store that refused what it was
    /// given: its own error says which.
    pub fn is_damage(&self) -> bool {
        matches!(
            self,
            Self::Change { .. }
                | Self::Rebuilt { .. }
                | Self::LogBehindCheckpoint { .. }
                | Self::CommitsMissing { .. }
                | Self::LinksAfterLanding { .. }
                | Self::MarkMissing { .. }
                | Self::WrongMark { .. }
                | Self::ChangesWithoutPage { .. }
                | Self::ValueNotThere { .. }
                | Self::StepOnAnotherPage { .. }
                | Self::NamedValue { .. }
        )
    }
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let item = |at: &LogPlace| format!("offset {} of log file {}", at.offset(), at.file());
        match self {
            Self::Segments(source) => write!(f, "recovery could not go on: {source}"),
            Self::Table(source) => write!(f, "the table checkpoint did not load: {source}"),
            Self::Values(source) => write!(f, "the value log was not recovered: {source}"),
            Self::Log(source) => write!(f, "the log was not recovered: {source}"),
            Self::Pool(source) => write!(f, "the buffer pool refused a rebuilt page: {source}"),
            Self::Step { page, source } => write!(
                f,
                "the buffer pool refused the rebuilt frame of page {page}: {source}"
            ),
            Self::Allocator(source) => write!(f, "the page ids were not recovered: {source}"),
            Self::Loader { source } => write!(f, "the store's pages cannot be read: {source}"),
            Self::Page { page, at, source } => write!(
                f,
                "page {page}, which the item at {} changes, could not be read: {source}",
                item(at)
            ),
            Self::Change { page, at, source } => write!(
                f,
                "the item at {} does not apply to page {page}: {source}",
                item(at)
            ),
            Self::Rebuilt { page, source } => write!(
                f,
                "page {page} is no page of its format as the log leaves it: {source}"
            ),
            Self::Link { page, source } => write!(
                f,
                "the page table refused a link of page {page} that replay made: {source}"
            ),
            Self::LogBehindCheckpoint { covers, landed } => write!(
                f,
                "the log ends at commit {landed}, and the table checkpoint covers commit {covers}"
            ),
            Self::CommitsMissing { covers, found } => write!(
                f,
                "the log goes on from commit {found}, and the table checkpoint covers commit \
                 {covers}: the commits between them are missing"
            ),
            Self::LinksAfterLanding { links, landed } => write!(
                f,
                "the table checkpoint holds {links} links of versions after {landed}, the last \
                 commit of the log"
            ),
            Self::MarkMissing { place } => write!(
                f,
                "no checkpoint mark ends at {}, the place of the table checkpoint's sample",
                item(place)
            ),
            Self::WrongMark {
                place,
                mark,
                covers,
            } => write!(
                f,
                "the mark that ends at {} is of a checkpoint that covers commit {mark}, and the \
                 table checkpoint covers commit {covers}",
                item(place)
            ),
            Self::ChangesWithoutPage { page, pages } => write!(
                f,
                "the log changes {pages} pages that the table checkpoint has no image of and \
                 that no later commit freed or made again, page {page} among them"
            ),
            Self::ValueNotThere { pointer, at, why } => write!(
                f,
                "the item at {} makes a cell hold a value at offset {} of value-log file {}: \
                 {why}",
                item(at),
                pointer.offset(),
                pointer.file()
            ),
            Self::StepOnAnotherPage { page, at } => write!(
                f,
                "the reclaiming step at {} frees page {page}, which is in use and does not hold \
                 what the step took out of it",
                item(at)
            ),
            Self::NamedValue { page } => write!(
                f,
                "a pending value of page {page} is named by the log and not carried in it"
            ),
        }
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segments(source) => Some(source),
            Self::Table(source) => Some(source),
            Self::Values(source) => Some(source),
            Self::Log(source) => Some(source),
            Self::Pool(source) => Some(source),
            Self::Step { source, .. } => Some(source),
            Self::Allocator(source) => Some(source),
            Self::Loader { source } => Some(source),
            Self::Page { source, .. } => Some(source),
            Self::Change { source, .. } => Some(source),
            Self::Rebuilt { source, .. } => Some(source),
            Self::Link { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<SegmentStoreError> for RecoveryError {
    fn from(source: SegmentStoreError) -> Self {
        Self::Segments(source)
    }
}

impl From<PageTableError> for RecoveryError {
    fn from(source: PageTableError) -> Self {
        Self::Table(source)
    }
}

impl From<ValueStoreError> for RecoveryError {
    fn from(source: ValueStoreError) -> Self {
        Self::Values(source)
    }
}

impl From<LogError> for RecoveryError {
    fn from(source: LogError) -> Self {
        Self::Log(source)
    }
}

impl From<CacheError> for RecoveryError {
    fn from(source: CacheError) -> Self {
        Self::Pool(source)
    }
}

impl From<AllocatorError> for RecoveryError {
    fn from(source: AllocatorError) -> Self {
        Self::Allocator(source)
    }
}
