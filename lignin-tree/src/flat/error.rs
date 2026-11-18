//! The error of a flat keyspace operation.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    alloc::AllocError, cache::CacheError, load::LoadError, values::ValueStoreError,
};

/// Why an operation on a flat keyspace failed.
///
/// A write that fails may have changed some pages of its writer and not
/// others. The writer's pages are then no state of the keyspace, and whoever
/// owns them gives them up (`transactions.md` WT10).
#[derive(Debug)]
pub enum FlatError {
    /// The page codec refused: bytes that are not a flat page, a value's
    /// place that is not one, or a page that could not be built.
    Page(FormatError),
    /// A committed page was not loaded.
    Load(LoadError),
    /// The buffer pool refused a private copy.
    Cache(CacheError),
    /// The value log did not give a value.
    Values(ValueStoreError),
    /// No page id was given out, or one was not taken back.
    Alloc(AllocError),
    /// The pages do not form a tree: what a page holds does not fit what
    /// the page above it says.
    Damaged {
        /// The page that was found wrong.
        page: PageId,
        /// What is wrong with it.
        what: &'static str,
    },
    /// A value is longer than the value log can hold.
    ValueTooLong {
        /// The value's length in bytes.
        len: usize,
    },
    /// The longest value kept in a page was set outside what a page takes.
    InlineLimit {
        /// The limit asked for.
        limit: usize,
        /// The smallest limit allowed.
        min: usize,
        /// The largest limit allowed for the page size.
        max: usize,
    },
    /// An entry's value is pending, and its bytes are not with the page.
    NoValue {
        /// The page whose entry holds the pending value.
        page: PageId,
    },
    /// A page was asked of the writer that freed it.
    Freed {
        /// The page.
        page: PageId,
    },
}

impl fmt::Display for FlatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page(error) => write!(f, "flat page: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::Cache(error) => write!(f, "buffer pool: {error}"),
            Self::Values(error) => write!(f, "value log: {error}"),
            Self::Alloc(error) => write!(f, "page ids: {error}"),
            Self::Damaged { page, what } => write!(f, "flat page {page}: {what}"),
            Self::ValueTooLong { len } => {
                write!(
                    f,
                    "a value of {len} bytes is longer than the value log holds"
                )
            }
            Self::InlineLimit { limit, min, max } => write!(
                f,
                "an inline limit of {limit} bytes is not between {min} and {max}"
            ),
            Self::NoValue { page } => write!(
                f,
                "page {page} holds a pending value whose bytes are not with it"
            ),
            Self::Freed { page } => write!(f, "page {page} was freed by this writer"),
        }
    }
}

impl Error for FlatError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Page(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::Values(error) => Some(error),
            Self::Alloc(error) => Some(error),
            Self::Damaged { .. }
            | Self::ValueTooLong { .. }
            | Self::InlineLimit { .. }
            | Self::NoValue { .. }
            | Self::Freed { .. } => None,
        }
    }
}

impl From<FormatError> for FlatError {
    fn from(error: FormatError) -> Self {
        Self::Page(error)
    }
}

impl From<LoadError> for FlatError {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}

impl From<CacheError> for FlatError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<ValueStoreError> for FlatError {
    fn from(error: ValueStoreError) -> Self {
        Self::Values(error)
    }
}

impl From<AllocError> for FlatError {
    fn from(error: AllocError) -> Self {
        Self::Alloc(error)
    }
}
