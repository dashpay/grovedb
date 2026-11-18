//! The errors the page id allocator returns.

use std::fmt;

use lignin_format::{CommitVersion, PageId};

use super::PageState;

/// Why the allocator refused a call. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Every page id is in use, or is waiting until it may be used again.
    Exhausted,
    /// The id is not one that an open transaction holds, so it can be
    /// neither published nor given back.
    NotPrivate {
        /// The id.
        page: PageId,
        /// The state it is in.
        state: PageState,
    },
    /// The id does not name a committed page, so no commit can free it.
    NotLive {
        /// The id.
        page: PageId,
        /// The state it is in.
        state: PageState,
    },
    /// The page was said to be freed at version 0. That is the empty
    /// database, which no commit produces and which holds no page.
    FreedByNoCommit {
        /// The id.
        page: PageId,
    },
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => write!(f, "no page id is left"),
            Self::NotPrivate { page, state } => write!(
                f,
                "page id {page} is {state}, not held by an open transaction"
            ),
            Self::NotLive { page, state } => {
                write!(f, "page id {page} is {state}, not a committed page")
            }
            Self::FreedByNoCommit { page } => {
                write!(f, "page id {page} cannot be freed at version 0")
            }
        }
    }
}

impl std::error::Error for AllocError {}

/// Why the allocator of a recovered database could not be derived. What
/// recovery was told, or the page table it loaded, is not of the version it
/// landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryError {
    /// The newest link of a page in the table is of a later version than
    /// the one recovery landed on. A checkpoint is named only once the log
    /// is durable through every link it holds, so that rule was broken.
    LinkAfterLanding {
        /// The page.
        page: PageId,
        /// The version of its newest link.
        version: CommitVersion,
        /// The version recovery landed on.
        landed: CommitVersion,
    },
    /// Replay reported a page for a later version than the one recovery
    /// landed on: a commit that is not part of the database.
    ReportAfterLanding {
        /// The page.
        page: PageId,
        /// The version reported.
        version: CommitVersion,
        /// The version recovery landed on.
        landed: CommitVersion,
    },
    /// Replay reported a page for version 0, which no commit has.
    ReportOfNoCommit {
        /// The page.
        page: PageId,
    },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LinkAfterLanding {
                page,
                version,
                landed,
            } => write!(
                f,
                "the page table holds page {page} at version {version}, and recovery landed on \
                 version {landed}"
            ),
            Self::ReportAfterLanding {
                page,
                version,
                landed,
            } => write!(
                f,
                "replay reported page {page} for version {version}, and recovery landed on \
                 version {landed}"
            ),
            Self::ReportOfNoCommit { page } => {
                write!(f, "replay reported page {page} for version 0")
            }
        }
    }
}

impl std::error::Error for RecoveryError {}
