//! The error of an operation on a value.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    cache::CacheError,
    load::LoadError,
    values::{OwnersError, ValueStoreError},
};

use crate::{fragment::FragmentError, node::NodeError};

/// Why a value was not read, not written or not moved.
#[derive(Debug)]
pub enum ValueError {
    /// An inline threshold that a store of this page size cannot have.
    Threshold {
        /// The threshold that was asked for.
        threshold: usize,
        /// The smallest threshold: a place takes this many bytes of a cell.
        min: usize,
        /// The largest threshold that keeps every cell under half a page.
        max: usize,
    },
    /// A value longer than the value log holds.
    TooLong {
        /// The value's length.
        len: usize,
    },
    /// A cell of `page` holds a pending value whose bytes are not where
    /// they belong: not with the frame of the page, or not among the values
    /// of the write that holds the cell.
    NoValue {
        /// The page of the cell.
        page: PageId,
    },
    /// The values of a write hold bytes for `page` that no cell of the page
    /// holds a pending value for.
    NoCell {
        /// The page.
        page: PageId,
    },
    /// Two cells of `page` hold one pending value.
    Shared {
        /// The page.
        page: PageId,
    },
    /// A copy of a regrouped value is not covered by the durable manifest,
    /// so no page may point at it yet.
    NotCovered {
        /// The page whose values were copied.
        page: PageId,
    },
    /// The page codec or the log format refused.
    Format(FormatError),
    /// A node was not read or not changed.
    Node(NodeError),
    /// A page was not loaded.
    Load(LoadError),
    /// The buffer pool refused.
    Cache(CacheError),
    /// The value log failed.
    Store(ValueStoreError),
    /// The pages of a write failed.
    Pages(FragmentError),
    /// The log did not take the record of a page step.
    Log(OwnersError),
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Threshold {
                threshold,
                min,
                max,
            } => write!(
                f,
                "an inline threshold of {threshold} bytes is not between {min} and {max}"
            ),
            Self::TooLong { len } => write!(f, "a value of {len} bytes is too long"),
            Self::NoValue { page } => {
                write!(f, "the bytes of a pending value of page {page} are missing")
            }
            Self::NoCell { page } => {
                write!(
                    f,
                    "no cell of page {page} holds a value that is kept for it"
                )
            }
            Self::Shared { page } => {
                write!(f, "two cells of page {page} hold one pending value")
            }
            Self::NotCovered { page } => write!(
                f,
                "no durable manifest covers the regrouped values of page {page}"
            ),
            Self::Format(error) => write!(f, "format: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::Cache(error) => write!(f, "buffer pool: {error}"),
            Self::Store(error) => write!(f, "value log: {error}"),
            Self::Pages(error) => write!(f, "pages: {error}"),
            Self::Log(error) => write!(f, "log: {error}"),
        }
    }
}

impl Error for ValueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Format(error) => Some(error),
            Self::Node(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Pages(error) => Some(error),
            Self::Log(error) => Some(&**error),
            Self::Threshold { .. }
            | Self::TooLong { .. }
            | Self::NoValue { .. }
            | Self::NoCell { .. }
            | Self::Shared { .. }
            | Self::NotCovered { .. } => None,
        }
    }
}

impl From<FormatError> for ValueError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl From<NodeError> for ValueError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

impl From<LoadError> for ValueError {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}

impl From<CacheError> for ValueError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<ValueStoreError> for ValueError {
    fn from(error: ValueStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<FragmentError> for ValueError {
    fn from(error: FragmentError) -> Self {
        Self::Pages(error)
    }
}
