//! The errors of a bulk load.

use std::{error::Error, fmt};

use lignin_format::CellAddress;

use super::node::Loaded;
use crate::{fragment::FragmentError, hash::Refusal, node::NodeError};

/// Why a load stopped. A load that returned an error is over: every later
/// call returns [`LoadError::Failed`], and the caller rolls back the scope
/// the pages belong to.
#[derive(Debug)]
pub enum LoadError {
    /// The fill a load was asked for is no fill: the least is above the
    /// most, or the most is 0 or above 100.
    Fill {
        /// The least fill, in percent.
        least: u32,
        /// The most fill, in percent.
        most: u32,
    },
    /// A node, or a call, names a subtree that is not one of this load
    /// that nothing points at: another node took it, it was placed as a
    /// root, or this load never made it. A node that names one subtree
    /// twice gets this for the second.
    NotLoaded {
        /// The subtree that was named.
        subtree: Loaded,
    },
    /// The node's cell was refused: a part of it is too long for a cell,
    /// or the cell for a page.
    Node(NodeError),
    /// A rule refused to make a hash.
    Rule(Refusal),
    /// The values refused a value.
    Value(Refusal),
    /// The pages could not be reached, or a page refused a cell.
    Pages(FragmentError),
    /// A hosted root that the node names by its address is not there, and
    /// the node's value hash is made from its hash.
    NoRoot {
        /// The address.
        address: CellAddress,
    },
    /// A page gave a cell another slot than the load counted: the page was
    /// not empty when it was started, or something else wrote to it.
    Slot {
        /// The address the load counted.
        expected: CellAddress,
        /// The address the page gave.
        found: CellAddress,
    },
    /// A stream in key order does not describe a tree.
    Shape(ShapeError),
    /// The load was finished with subtrees that nothing points at and that
    /// were not placed as roots.
    Unfinished {
        /// How many.
        subtrees: usize,
    },
    /// The load returned an error before.
    Failed,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fill { least, most } => {
                write!(f, "no fill: at least {least}% and at most {most}%")
            }
            Self::NotLoaded { subtree } => {
                write!(f, "{subtree} is no subtree of the load that is free")
            }
            Self::Node(error) => write!(f, "cell: {error}"),
            Self::Rule(error) => write!(f, "hash rule: {error}"),
            Self::Value(error) => write!(f, "value: {error}"),
            Self::Pages(error) => write!(f, "pages: {error}"),
            Self::NoRoot { address } => write!(f, "no hosted root at {address}"),
            Self::Slot { expected, found } => {
                write!(f, "a cell counted for {expected} was put at {found}")
            }
            Self::Shape(error) => write!(f, "shape: {error}"),
            Self::Unfinished { subtrees } => {
                write!(f, "{subtrees} subtrees of the load have no place")
            }
            Self::Failed => f.write_str("the load failed before"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Node(error) => Some(error),
            Self::Rule(error) | Self::Value(error) => Some(error.as_ref()),
            Self::Pages(error) => Some(error),
            Self::Shape(error) => Some(error),
            Self::Fill { .. }
            | Self::NotLoaded { .. }
            | Self::NoRoot { .. }
            | Self::Slot { .. }
            | Self::Unfinished { .. }
            | Self::Failed => None,
        }
    }
}

impl From<NodeError> for LoadError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

impl From<FragmentError> for LoadError {
    fn from(error: FragmentError) -> Self {
        Self::Pages(error)
    }
}

impl From<ShapeError> for LoadError {
    fn from(error: ShapeError) -> Self {
        Self::Shape(error)
    }
}

/// What is wrong with a stream of nodes in key order
/// ([`InOrder`](super::InOrder)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    /// A node has a left child, and no subtree ends right before it.
    NoLeft,
    /// A node has a left child, and the subtree that ends right before it
    /// has other heights than the node's link states.
    LeftHeights {
        /// The heights the link states.
        stated: (u8, u8),
        /// The heights of the subtree's root.
        found: (u16, u16),
    },
    /// A subtree ends right before a node that has no left child, and it is
    /// not the right child of a node before it.
    NoParent,
    /// The stream ended while nodes still waited for their right child.
    NoRight {
        /// How many.
        waiting: usize,
    },
}

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLeft => f.write_str("a node has a left child that the stream did not give"),
            Self::LeftHeights { stated, found } => write!(
                f,
                "a left child of heights {stated:?} is stated, and one of {found:?} was given"
            ),
            Self::NoParent => f.write_str("a subtree is the child of no node"),
            Self::NoRight { waiting } => {
                write!(f, "{waiting} nodes have a right child that was not given")
            }
        }
    }
}

impl Error for ShapeError {}
