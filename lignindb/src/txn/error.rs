//! The errors of a write transaction.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    alloc::AllocError, cache::CacheError, load::LoadError, values::ValueStoreError,
};
use lignin_tree::{flat::FlatError, fragment::FragmentError, node::NodeError};

use super::key::{Subtree, Unit};

/// What is wrong with a [`Batch`](super::Batch). A batch that is refused
/// with one of these has changed nothing (`transactions.md` WT10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchError {
    /// A record key is written twice in one batch. A batch holds one
    /// outcome for a key (GM0).
    KeyTwice {
        /// The unit of the key.
        unit: Unit,
    },
    /// A Merk has two change sets in one batch.
    MerkTwice {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// The key of a node, or of a child a link names, is longer than a
    /// cell holds.
    KeyTooLong {
        /// The Merk's subtree.
        subtree: Subtree,
        /// The length of the key.
        len: usize,
    },
    /// A node hosts more Merks than a cell has pointers for.
    TooManyHosted {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A value is longer than the value log holds.
    ValueTooLong {
        /// The length of the value.
        len: usize,
    },
    /// A link names a child that the Merk does not hold in the
    /// transaction's view.
    NoChild {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// The root that the change set names is a key with no node.
    NoRoot {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node keeps a value, and the transaction's view has no node under
    /// its key.
    NoValue {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A range delete names a unit that the transaction cannot remove a
    /// range from: the Merk nodes of a subtree are dropped, not deleted by
    /// range (CV9, TREE-09).
    MerkRange {
        /// The unit.
        unit: Unit,
    },
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyTwice { unit } => write!(f, "a key of {unit} is written twice"),
            Self::MerkTwice { subtree } => {
                write!(f, "the Merk {subtree} has two change sets")
            }
            Self::KeyTooLong { subtree, len } => {
                write!(f, "a node key of {len} bytes in the Merk {subtree}")
            }
            Self::TooManyHosted { subtree } => {
                write!(f, "a node of the Merk {subtree} hosts too many Merks")
            }
            Self::ValueTooLong { len } => write!(f, "a value of {len} bytes"),
            Self::NoChild { subtree } => {
                write!(f, "a link names a child the Merk {subtree} does not hold")
            }
            Self::NoRoot { subtree } => {
                write!(f, "the root of the Merk {subtree} is a key with no node")
            }
            Self::NoValue { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} keeps a value it does not have"
                )
            }
            Self::MerkRange { unit } => {
                write!(f, "a range of Merk nodes is deleted in {unit}")
            }
        }
    }
}

impl Error for BatchError {}

/// Why an operation of a write transaction failed. The transaction's write
/// set is what it was before the call (`transactions.md` WT10).
#[derive(Debug)]
pub enum TxnError {
    /// The batch was refused.
    Batch(BatchError),
    /// Another scope of the transaction is open. Until parallel scopes
    /// exist (TXN-09), a transaction has one scope at a time, and a read
    /// outside it waits for none: it is refused.
    ScopeOpen,
    /// The tree layer did not apply a write.
    Fragment(FragmentError),
    /// A tree page was not read.
    Node(NodeError),
    /// A flat keyspace was not read or not written.
    Flat(FlatError),
    /// The buffer pool refused a private copy.
    Cache(CacheError),
    /// No page id was given, or one was not taken back.
    Alloc(AllocError),
    /// A page was not loaded.
    Load(LoadError),
    /// A value was not read from the value log.
    Values(ValueStoreError),
    /// Bytes that should be a page or a value are not.
    Format(FormatError),
    /// The subtree directory did not answer.
    Directory(Box<dyn Error + Send + Sync>),
    /// A tree page that the transaction freed was asked for again.
    PageFreed {
        /// The page.
        page: PageId,
    },
    /// The pages do not hold what every committed state holds.
    Damaged(&'static str),
    /// A value that is out of line and that no value-log file holds yet is
    /// needed in bytes, and it is not one the transaction wrote. Reading
    /// such a value from the frame of its page is the read path of TREE-08.
    PendingValue {
        /// The Merk of the node that holds the value.
        subtree: Subtree,
    },
}

impl fmt::Display for TxnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Batch(error) => write!(f, "batch: {error}"),
            Self::ScopeOpen => f.write_str("another scope of the transaction is open"),
            Self::Fragment(error) => write!(f, "tree pages: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Cache(error) => write!(f, "buffer pool: {error}"),
            Self::Alloc(error) => write!(f, "page ids: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::Values(error) => write!(f, "value log: {error}"),
            Self::Format(error) => write!(f, "format: {error}"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::PageFreed { page } => write!(f, "{page} was freed by the transaction"),
            Self::Damaged(what) => write!(f, "damaged pages: {what}"),
            Self::PendingValue { subtree } => {
                write!(
                    f,
                    "a pending value of the Merk {subtree} is not the transaction's to read"
                )
            }
        }
    }
}

impl Error for TxnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Batch(error) => Some(error),
            Self::Fragment(error) => Some(error),
            Self::Node(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::Alloc(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Values(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Directory(error) => Some(error.as_ref()),
            Self::ScopeOpen
            | Self::PageFreed { .. }
            | Self::Damaged(_)
            | Self::PendingValue { .. } => None,
        }
    }
}

macro_rules! from {
    ($($source:ty => $variant:ident),* $(,)?) => {$(
        impl From<$source> for TxnError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        }
    )*};
}

from! {
    BatchError => Batch,
    FragmentError => Fragment,
    NodeError => Node,
    FlatError => Flat,
    CacheError => Cache,
    AllocError => Alloc,
    LoadError => Load,
    ValueStoreError => Values,
    FormatError => Format,
}

impl TxnError {
    /// The error as the tree layer takes one from the pages of a write
    /// (`FragmentError::Pages`).
    pub(crate) fn into_pages(self) -> FragmentError {
        match self {
            Self::Fragment(error) => error,
            Self::Node(error) => FragmentError::Node(error),
            other => FragmentError::Pages(Box::new(other)),
        }
    }
}
