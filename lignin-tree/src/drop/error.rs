//! The error of a drop, and of reclaiming what one left.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    load::LoadError,
    values::{OwnersError, ValueStoreError},
};

use crate::{directory::DirectoryError, flat::FlatError, fragment::FragmentError};

/// Why a drop, or a step of reclaiming, failed.
///
/// A step that failed with [`Self::Damaged`], [`Self::Format`],
/// [`Self::Load`], [`Self::Directory`] or [`Self::Flat`] has changed
/// nothing. [`Self::Store`] and [`Self::Values`] can come after the step's
/// record is in the log: what was being reclaimed is then given up, as
/// [`Reclaim`](super::Reclaim) says.
#[derive(Debug)]
pub enum DropError {
    /// The subtree directory refused, or could not be read.
    Directory(DirectoryError),
    /// A flat keyspace could not be read or written.
    Flat(FlatError),
    /// The write to the tree pages failed.
    Fragment(FragmentError),
    /// Bytes that are not the page or the record they were read as.
    Format(FormatError),
    /// A page could not be loaded.
    Load(LoadError),
    /// The counts of live values could not be changed as reported.
    Values(ValueStoreError),
    /// The log, the buffer pool, the page table or the page ids refused a
    /// step.
    Store(OwnersError),
    /// What a dropped subtree left is not as a drop leaves it: the walk met
    /// something that no cell of it can be.
    Damaged {
        /// The page it was found in.
        page: PageId,
        /// What is wrong.
        what: &'static str,
    },
    /// A record of unlinked pages that is not one.
    Record(&'static str),
    /// Every id of a record of unlinked pages was given out.
    IdsExhausted,
    /// Reclaiming was asked to go on with something that an earlier error
    /// gave up.
    GivenUp,
}

impl fmt::Display for DropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory(error) => write!(f, "subtree drop: {error}"),
            Self::Flat(error) => write!(f, "subtree drop: {error}"),
            Self::Fragment(error) => write!(f, "subtree drop: {error}"),
            Self::Format(error) => write!(f, "subtree drop: {error}"),
            Self::Load(error) => write!(f, "subtree drop: {error}"),
            Self::Values(error) => write!(f, "subtree drop: {error}"),
            Self::Store(error) => write!(f, "subtree drop: {error}"),
            Self::Damaged { page, what } => {
                write!(f, "what a dropped subtree left in page {page}: {what}")
            }
            Self::Record(what) => write!(f, "record of unlinked pages: {what}"),
            Self::IdsExhausted => write!(f, "every id of a record of unlinked pages was given out"),
            Self::GivenUp => write!(f, "reclaiming was given up after an error"),
        }
    }
}

impl Error for DropError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Directory(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Fragment(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Values(error) => Some(error),
            Self::Store(error) => Some(&**error),
            _ => None,
        }
    }
}

impl From<DirectoryError> for DropError {
    fn from(error: DirectoryError) -> Self {
        Self::Directory(error)
    }
}

impl From<FlatError> for DropError {
    fn from(error: FlatError) -> Self {
        Self::Flat(error)
    }
}

impl From<FragmentError> for DropError {
    fn from(error: FragmentError) -> Self {
        Self::Fragment(error)
    }
}

impl From<FormatError> for DropError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl From<LoadError> for DropError {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}

impl From<ValueStoreError> for DropError {
    fn from(error: ValueStoreError) -> Self {
        Self::Values(error)
    }
}
