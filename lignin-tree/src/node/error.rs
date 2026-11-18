//! The error of a node operation.

use std::{error::Error, fmt};

use lignin_format::{CellAddress, FormatError};
use lignin_store::load::LoadError;

/// Why a node was not read or not changed.
#[derive(Debug)]
pub enum NodeError {
    /// The page codec refused: the frame does not hold a tree page, or the
    /// page cannot take the change. When a change is refused, the page is as
    /// it was.
    Page(FormatError),
    /// The page was not loaded.
    Load(LoadError),
    /// An address that a page stores names a slot that holds no cell.
    NoNode {
        /// The address.
        address: CellAddress,
    },
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page(error) => write!(f, "tree page: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::NoNode { address } => write!(f, "no node at {address}"),
        }
    }
}

impl Error for NodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Page(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::NoNode { .. } => None,
        }
    }
}

impl From<FormatError> for NodeError {
    fn from(error: FormatError) -> Self {
        Self::Page(error)
    }
}

impl From<LoadError> for NodeError {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}
