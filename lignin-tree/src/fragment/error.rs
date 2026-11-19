//! The errors of fragment maintenance.

use std::{error::Error, fmt};

use lignin_format::{CellAddress, PageId};

use crate::node::NodeError;

/// Why a write was not applied.
#[derive(Debug)]
pub enum FragmentError {
    /// A page was not read or not changed.
    Node(NodeError),
    /// The pages could not be reached: a page was not made private, no page
    /// id was given, a page was not freed. It is the error of whoever
    /// implements [`Pages`](super::Pages).
    Pages(Box<dyn Error + Send + Sync>),
    /// The write does not describe a tree the pages can hold. No page was
    /// changed.
    Write(WriteError),
    /// A page is above its capacity and no rule can take a cell out of it:
    /// what the write added to it is a node that nothing in the write points
    /// at, in a page whose other cells the write does not hold
    /// (`tree-layout.md` T3). No page was changed.
    NoRoom {
        /// The page; `None` for a page the write would have started.
        page: Option<PageId>,
    },
}

impl fmt::Display for FragmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(error) => write!(f, "tree page: {error}"),
            Self::Pages(error) => write!(f, "pages: {error}"),
            Self::Write(error) => write!(f, "write: {error}"),
            Self::NoRoom { page: Some(page) } => {
                write!(f, "{page} is full of cells the write does not hold")
            }
            Self::NoRoom { page: None } => f.write_str("a new page is above its capacity"),
        }
    }
}

impl Error for FragmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Node(error) => Some(error),
            Self::Pages(error) => Some(error.as_ref()),
            Self::Write(error) => Some(error),
            Self::NoRoom { .. } => None,
        }
    }
}

impl From<NodeError> for FragmentError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

impl From<WriteError> for FragmentError {
    fn from(error: WriteError) -> Self {
        Self::Write(error)
    }
}

/// What is wrong with a [`Write`](super::Write).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    /// An address the write gives names a slot that holds no cell.
    NoNode {
        /// The address.
        address: CellAddress,
    },
    /// The cell at the address of a written node has another key: it is
    /// another node.
    KeyChanged {
        /// The node, by its position in the write.
        node: usize,
    },
    /// One cell is written twice, or removed twice, or written or let go of
    /// and removed.
    Twice {
        /// The cell's address.
        address: CellAddress,
    },
    /// Two links or pointers of the write name one node, or a node names
    /// itself.
    TwoPointers {
        /// The address of the node that is named twice; `None` for a node
        /// the write creates.
        address: Option<CellAddress>,
    },
    /// A link or pointer names a node of the write that the write does not
    /// have.
    NoSuchNode {
        /// The position that was named.
        node: usize,
    },
    /// A link or pointer names a cell that the write removes, or a cell that
    /// the write does not hold still points at one.
    Removed {
        /// The removed cell's address.
        address: CellAddress,
    },
    /// A link or pointer of the write names a cell that the write lets go
    /// of.
    Dropped {
        /// The cell's address.
        address: CellAddress,
    },
    /// A link or pointer keeps what the cell has, and the cell has nothing
    /// there, or the node is new.
    NothingKept {
        /// The node, by its position in the write.
        node: usize,
    },
    /// A node the write creates keeps a value it never had.
    NoValue {
        /// The node, by its position in the write.
        node: usize,
    },
    /// The nodes of the write point at each other in a circle.
    Cycle,
    /// A node's cell would not be smaller than half a page
    /// (`tree-layout.md` C7).
    CellTooLarge {
        /// The node, by its position in the write.
        node: usize,
        /// The bytes its cell would take.
        len: usize,
        /// The most a cell may take.
        max: usize,
    },
    /// A part of a node is longer than its length field holds.
    FieldTooLong {
        /// The node, by its position in the write.
        node: usize,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNode { address } => write!(f, "no node at {address}"),
            Self::KeyChanged { node } => {
                write!(
                    f,
                    "node {node} has another key than the cell at its address"
                )
            }
            Self::Twice { address } => write!(f, "the cell at {address} is named twice"),
            Self::TwoPointers {
                address: Some(address),
            } => {
                write!(f, "two pointers name the node at {address}")
            }
            Self::TwoPointers { address: None } => f.write_str("two pointers name one new node"),
            Self::NoSuchNode { node } => write!(f, "the write has no node {node}"),
            Self::Removed { address } => {
                write!(
                    f,
                    "a pointer names the cell at {address}, which the write removes"
                )
            }
            Self::Dropped { address } => {
                write!(
                    f,
                    "a pointer names the cell at {address}, which the write lets go of"
                )
            }
            Self::NothingKept { node } => {
                write!(f, "node {node} keeps a pointer its cell does not have")
            }
            Self::NoValue { node } => write!(f, "the new node {node} has no value"),
            Self::Cycle => f.write_str("the nodes of the write point at each other in a circle"),
            Self::CellTooLarge { node, len, max } => {
                write!(f, "the cell of node {node} takes {len} bytes, above {max}")
            }
            Self::FieldTooLong { node } => write!(f, "a part of node {node} is too long"),
        }
    }
}

impl Error for WriteError {}

/// A layout parameter that breaks a rule of `tree-layout.md`, section 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// A small fragment is at most half a page: the divisor is below 2.
    SmallFragment,
    /// The largest fragment is 10 to 100 percent of what a page holds.
    LargestFragment,
    /// Three small fragments do not fit in a largest one, so half of a
    /// fragment that was split would be carried straight back.
    SmallAgainstLargest,
    /// The seed limit is above what a page holds.
    SeedLimit,
    /// The merge threshold is above the seed limit, so a page that gives a
    /// fragment away could not take one.
    MergeThreshold,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SmallFragment => "a small fragment is at most half a page",
            Self::LargestFragment => "the largest fragment is 10 to 100 percent of a page",
            Self::SmallAgainstLargest => "three small fragments must fit in a largest one",
            Self::SeedLimit => "the seed limit is at most a full page",
            Self::MergeThreshold => "the merge threshold is at most the seed limit",
        })
    }
}

impl Error for LayoutError {}
