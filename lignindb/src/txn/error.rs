//! The errors of a write transaction.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    alloc::AllocError, cache::CacheError, load::LoadError, values::ValueStoreError,
};
use lignin_tree::{
    cursor::CursorError, directory::DirectoryError, flat::FlatError, fragment::FragmentError,
    node::NodeError, value::ValueError,
};

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
    /// The root that the change set names is a key with no node: the Merk
    /// holds none under it in the transaction's view, or the change set
    /// itself deletes it.
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
    /// The transaction made the first node of a Merk that was empty, and a
    /// commit outside it has made one since, with no key in common. The
    /// two roots cannot both stand, and the conflict rule did not see it:
    /// whoever writes the first node of a Merk also writes a record that
    /// every such writer writes, which on existing GroveVersions is the
    /// element that hosts the Merk or the base root key (GM1, GM3).
    RootTaken {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A change set sets the root of a Merk, and writes and deletes no node
    /// of it. A root changes only with a node, and the root of a Merk that
    /// is as it was is kept ([`RootPut::Keep`](super::RootPut::Keep)).
    RootAlone {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A flat record is written, or a range deleted, in the data column of
    /// a subtree that holds Merk nodes there: in the batch, in the
    /// transaction's write set, or in the state it reads. A node is written
    /// through the change set of its Merk, and a Merk is dropped, not
    /// deleted by range (ST1, CV9, TREE-09).
    NotFlat {
        /// The unit.
        unit: Unit,
    },
    /// A change set names a Merk in whose data column the transaction has
    /// written flat records (ST1).
    NotMerk {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node is written that keeps the Merks it hosts
    /// ([`Hosted::Keep`](super::Hosted::Keep)), and the batch holds the
    /// change set of one of them. The write that holds a Merk and its host
    /// names the Merk in the host.
    HostKeeps {
        /// The subtree of the hosted Merk.
        subtree: Subtree,
    },
    /// A change set, a drop or a clear names a subtree that is not entered
    /// in the transaction's view, or a subtree is entered whose host is in
    /// one that is not. A subtree is entered by the batch that writes the
    /// element which hosts it
    /// ([`SubtreeWrite::Enter`](super::SubtreeWrite::Enter)).
    NotEntered {
        /// The subtree that is not entered.
        subtree: Subtree,
    },
    /// A subtree is entered that is entered already in the transaction's
    /// view, and that the batch does not drop.
    Entered {
        /// The subtree.
        subtree: Subtree,
    },
    /// A batch enters one subtree twice, or drops or clears it twice.
    SubtreeTwice {
        /// The subtree.
        subtree: Subtree,
    },
    /// The tree type of an entered subtree is longer than the subtree
    /// directory keeps.
    TreeTypeTooLong {
        /// The subtree.
        subtree: Subtree,
        /// The length of the tree type.
        len: usize,
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
            Self::RootTaken { subtree } => {
                write!(
                    f,
                    "the empty Merk {subtree} was given a root outside the transaction"
                )
            }
            Self::RootAlone { subtree } => {
                write!(f, "the root of the Merk {subtree} is set with no node")
            }
            Self::NotFlat { unit } => {
                write!(f, "a flat record is written among the Merk nodes in {unit}")
            }
            Self::NotMerk { subtree } => {
                write!(f, "the Merk {subtree} is written among flat records")
            }
            Self::HostKeeps { subtree } => {
                write!(
                    f,
                    "the host of the Merk {subtree} keeps its pointers in a batch that writes the Merk"
                )
            }
            Self::NotEntered { subtree } => write!(f, "the subtree {subtree} is not entered"),
            Self::Entered { subtree } => {
                write!(f, "the subtree {subtree} is entered already")
            }
            Self::SubtreeTwice { subtree } => {
                write!(f, "the subtree {subtree} is entered, or leaves, twice")
            }
            Self::TreeTypeTooLong { subtree, len } => {
                write!(f, "a tree type of {len} bytes for the subtree {subtree}")
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
    /// A scope of the transaction is open, and the call needs every scope
    /// closed: a rollback of the transaction, a rollback to a mark, or its
    /// commit (`transactions.md` SC8, CM1; the specification's
    /// `ScopesOpen`). The transaction is as it was.
    ScopeOpen,
    /// The operation touches a unit that another open scope of the
    /// transaction holds: it writes where that scope has read or written,
    /// or reads where that scope has written (SC3). A claim never waits.
    /// The operation had no effect, and its scope stays open and usable.
    ScopeOverlap {
        /// The unit that the other scope holds: the one the operation
        /// names, or a subtree above it that the other scope has dropped
        /// or cleared.
        unit: Unit,
    },
    /// The transaction has a pending mark left: its tree is not finalized
    /// up to the root, and it is not committed so (SC9, CM1). The
    /// transaction is as it was.
    NotFinalized,
    /// The tree layer did not apply a write.
    Fragment(FragmentError),
    /// A tree page was not read.
    Node(NodeError),
    /// The committed side of a cursor over the nodes of a Merk did not
    /// move.
    Cursor(CursorError),
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
    /// A value of a node that is not in its cell was not kept or not read
    /// (`lignin_tree::value`).
    TreeValue(ValueError),
    /// Bytes that should be a page or a value are not.
    Format(FormatError),
    /// The subtree directory did not answer, or refused a write
    /// (`lignin_tree::directory`).
    Directory(DirectoryError),
    /// A tree page that the transaction freed was asked for again.
    PageFreed {
        /// The page.
        page: PageId,
    },
    /// The pages do not hold what every committed state holds.
    Damaged(&'static str),
}

impl fmt::Display for TxnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Batch(error) => write!(f, "batch: {error}"),
            Self::ScopeOpen => f.write_str("a scope of the transaction is open"),
            Self::ScopeOverlap { unit } => {
                write!(f, "another open scope of the transaction holds {unit}")
            }
            Self::NotFinalized => f.write_str("the transaction has a pending mark left"),
            Self::Fragment(error) => write!(f, "tree pages: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Cursor(error) => write!(f, "cursor: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Cache(error) => write!(f, "buffer pool: {error}"),
            Self::Alloc(error) => write!(f, "page ids: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::Values(error) => write!(f, "value log: {error}"),
            Self::TreeValue(error) => write!(f, "node value: {error}"),
            Self::Format(error) => write!(f, "format: {error}"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::PageFreed { page } => write!(f, "{page} was freed by the transaction"),
            Self::Damaged(what) => write!(f, "damaged pages: {what}"),
        }
    }
}

impl Error for TxnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Batch(error) => Some(error),
            Self::Fragment(error) => Some(error),
            Self::Node(error) => Some(error),
            Self::Cursor(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::Alloc(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Values(error) => Some(error),
            Self::TreeValue(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Directory(error) => Some(error),
            Self::ScopeOpen
            | Self::ScopeOverlap { .. }
            | Self::NotFinalized
            | Self::PageFreed { .. }
            | Self::Damaged(_) => None,
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
    CursorError => Cursor,
    FlatError => Flat,
    CacheError => Cache,
    AllocError => Alloc,
    LoadError => Load,
    ValueStoreError => Values,
    ValueError => TreeValue,
    FormatError => Format,
    DirectoryError => Directory,
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
