//! The errors the buffer pool returns.

use std::fmt;

use lignin_format::{CommitVersion, PageId};

use super::OwnerId;

/// Why the pool refused a call. Nothing was changed.
///
/// Except for the two about the pool's size, each of these says that the
/// caller broke a rule of the module documentation: the pool was told
/// something that cannot be true of the pages it holds, or was handed
/// something it cannot take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheError {
    /// The configured size does not hold one frame.
    CapacityTooSmall {
        /// The configured size in bytes.
        capacity: usize,
        /// The bytes a pool of one frame takes: a page, its header, the
        /// pool's table and a lane.
        frame: usize,
    },
    /// The configured size is more than one allocation can be.
    CapacityTooLarge,
    /// A page was loaded for a reader at a version before the one that bore
    /// it, so that reader cannot see it.
    LoadedTooNew {
        /// The page.
        page: PageId,
        /// The version that bore what was loaded.
        born: CommitVersion,
        /// The version the reader asked at.
        at: CommitVersion,
    },
    /// A page was loaded for a reader at a version at which a newer version
    /// of the page is known, so that reader cannot see it.
    LoadedSuperseded {
        /// The page.
        page: PageId,
        /// The version that bore what was loaded.
        born: CommitVersion,
        /// The version of the page that follows it.
        superseded: CommitVersion,
        /// The version the reader asked at.
        at: CommitVersion,
    },
    /// The handle is of another pool: it was given to the pool it does not
    /// belong to.
    WrongPool,
    /// An operation still holds the latch of the private copy, so the copy
    /// can be neither installed nor discarded now.
    Latched {
        /// The page.
        page: PageId,
    },
    /// The owner already holds a private copy of the page.
    AlreadyPrivate {
        /// The page.
        page: PageId,
        /// The owner.
        owner: OwnerId,
    },
    /// The frame is not a private copy any more: it was installed by a
    /// commit, or discarded.
    NotPrivate {
        /// The page.
        page: PageId,
    },
    /// A commit installed a private copy over a page version that is not
    /// the one the copy was made from.
    BaseNotCurrent {
        /// The page.
        page: PageId,
        /// The version that bore what the copy was made from, or nothing for
        /// a page the transaction created.
        base: Option<CommitVersion>,
        /// The version that bore the current page version.
        current: Option<CommitVersion>,
    },
    /// A commit named a version that is not after the one that bore the
    /// page version it supersedes, or named version 0, which no commit has.
    VersionNotAfter {
        /// The page.
        page: PageId,
        /// The version the commit named.
        version: CommitVersion,
        /// The version that bore the current page version, if there is one.
        current: Option<CommitVersion>,
    },
    /// The frame holds a page version that write-back does not owe: it was
    /// written back before, or it is held for readers only.
    NotUnwritten {
        /// The page.
        page: PageId,
        /// The version that bore it.
        born: CommitVersion,
    },
    /// The frame still has attached values, so its content is not all in a
    /// segment.
    AttachedValues {
        /// The page.
        page: PageId,
        /// The version that bore it.
        born: CommitVersion,
    },
    /// The frame is not how its page version is stored any more: a page
    /// step put another in its place.
    Replaced {
        /// The page.
        page: PageId,
        /// The version that bore it.
        born: CommitVersion,
    },
    /// The page version is the current one of its page, so write-back still
    /// owes it and it cannot be held for readers only.
    StillCurrent {
        /// The page.
        page: PageId,
        /// The version that bore it.
        born: CommitVersion,
    },
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let optional = |version: &Option<CommitVersion>| match version {
            Some(version) => format!("version {version}"),
            None => "no version".to_owned(),
        };
        match self {
            Self::CapacityTooSmall { capacity, frame } => write!(
                f,
                "a buffer pool of {capacity} bytes does not hold one frame of {frame} bytes"
            ),
            Self::CapacityTooLarge => write!(f, "the buffer pool is too large to allocate"),
            Self::LoadedTooNew { page, born, at } => write!(
                f,
                "page {page} was loaded as born at version {born} for a reader at version {at}"
            ),
            Self::LoadedSuperseded {
                page,
                born,
                superseded,
                at,
            } => write!(
                f,
                "page {page} was loaded as born at version {born} for a reader at version {at}, \
                 and it was superseded at version {superseded}"
            ),
            Self::WrongPool => write!(f, "the handle belongs to another buffer pool"),
            Self::Latched { page } => write!(
                f,
                "an operation still holds the latch of the private copy of page {page}"
            ),
            Self::AlreadyPrivate { page, owner } => {
                write!(f, "{owner} already holds a private copy of page {page}")
            }
            Self::NotPrivate { page } => {
                write!(f, "the frame of page {page} is not a private copy any more")
            }
            Self::BaseNotCurrent {
                page,
                base,
                current,
            } => write!(
                f,
                "a private copy of page {page} made from {} was installed over {}",
                optional(base),
                optional(current)
            ),
            Self::VersionNotAfter {
                page,
                version,
                current,
            } => write!(
                f,
                "a commit of version {version} cannot follow {} of page {page}",
                optional(current)
            ),
            Self::NotUnwritten { page, born } => write!(
                f,
                "page {page} as born at version {born} is not waiting to be written back"
            ),
            Self::AttachedValues { page, born } => write!(
                f,
                "page {page} as born at version {born} still has attached values"
            ),
            Self::Replaced { page, born } => write!(
                f,
                "the frame of page {page} as born at version {born} was replaced by a page step"
            ),
            Self::StillCurrent { page, born } => write!(
                f,
                "page {page} as born at version {born} is the current version of its page"
            ),
        }
    }
}

impl std::error::Error for CacheError {}

/// Why a page step installed nothing ([`PageStep::replace`]).
///
/// [`PageStep::replace`]: super::PageStep::replace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRefusal {
    /// The frame is not the current version of its page any more: a commit
    /// superseded or freed the page, or another page step replaced the
    /// frame.
    NotCurrent,
    /// An open write transaction holds a private copy of the page.
    PrivateCopy,
    /// The new frame is of another pool.
    WrongPool,
}

impl fmt::Display for StepRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCurrent => write!(f, "the frame is not the current version of its page"),
            Self::PrivateCopy => write!(
                f,
                "an open write transaction holds a private copy of the page"
            ),
            Self::WrongPool => write!(f, "the new frame belongs to another buffer pool"),
        }
    }
}

impl std::error::Error for StepRefusal {}
