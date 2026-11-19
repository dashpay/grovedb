//! The error of a drop, and of reclaiming what one left.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    load::LoadError,
    values::{OwnersError, ValueStoreError},
};

use crate::{
    directory::{DirectoryError, Prefix},
    flat::FlatError,
    fragment::FragmentError,
};

/// Why a drop, or a step of reclaiming, failed.
///
/// A step of reclaiming that failed has changed nothing, and is made again
/// later, unless the error is [`Self::Broken`]. A drop that failed says
/// what it left with its own documentation ([`unlink`](super::unlink)).
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
    /// The log did not take the record of a step. Nothing was changed.
    Store(OwnersError),
    /// The record of a step is in the log, and the buffer pool, the page
    /// table or the page ids refused what the record says: the store is
    /// not as its log has it. What was being reclaimed is given up.
    Broken {
        /// The page of the step.
        page: PageId,
        /// What refused.
        source: OwnersError,
    },
    /// What a dropped subtree left is not as a drop leaves it: the walk met
    /// something that no cell of it can be.
    Damaged {
        /// The page it was found in.
        page: PageId,
        /// What is wrong.
        what: &'static str,
    },
    /// A record of unlinked pages that is not one, or a key that no record
    /// of this module is under.
    Record(&'static str),
    /// A Merk was named to be dropped or cleared together with a Merk that
    /// it is below. The outer one takes it along.
    Below {
        /// The prefix of the inner Merk.
        prefix: Prefix,
    },
    /// Every id of a record of unlinked pages was given out.
    IdsExhausted,
    /// Reclaiming was asked to go on after [`Self::Broken`].
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
            Self::Broken { page, source } => write!(
                f,
                "the step on page {page} is in the log and was not made: {source}"
            ),
            Self::Damaged { page, what } => {
                write!(f, "what a dropped subtree left in page {page}: {what}")
            }
            Self::Record(what) => write!(f, "record of unlinked pages: {what}"),
            Self::Below { prefix } => {
                let [a, b, c, d, ..] = prefix;
                write!(
                    f,
                    "the Merk under the prefix {a:02x}{b:02x}{c:02x}{d:02x}.. is dropped with a \
                     Merk it is below"
                )
            }
            Self::IdsExhausted => write!(f, "every id of a record of unlinked pages was given out"),
            Self::GivenUp => write!(f, "reclaiming was given up after a step that broke"),
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
            Self::Store(error) | Self::Broken { source: error, .. } => Some(&**error),
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
