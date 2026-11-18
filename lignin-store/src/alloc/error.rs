//! The errors the page id allocator returns.

use std::fmt;

use lignin_format::PageId;

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
