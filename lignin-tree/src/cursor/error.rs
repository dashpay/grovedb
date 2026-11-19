//! The error of a cursor call.

use std::{error::Error, fmt};

use lignin_format::{CellAddress, CommitVersion};

use crate::node::{Lease, NodeError};

/// Why a cursor did not move or did not read.
#[derive(Debug)]
pub enum CursorError {
    /// A page on the way was not read, or an address that a page stores
    /// names no node.
    Node(NodeError),
    /// The call was given a reader of another version or another lease than
    /// the cursor was opened on. The cursor is as it was.
    OtherReader {
        /// The version the cursor was opened on.
        opened_at: CommitVersion,
        /// The lease the cursor was opened under.
        opened_under: Lease,
        /// The version of the reader that was given.
        given_at: CommitVersion,
        /// The lease of the reader that was given.
        given_under: Lease,
    },
    /// The way down from the root is longer than the way to any node of a
    /// tree whose heights a link can hold: the links of the tree lead back
    /// into it.
    TooDeep {
        /// The node at which the way was given up.
        at: CellAddress,
        /// The most nodes on one way down.
        limit: usize,
    },
    /// A step came to a node whose key is not on the side of the key it
    /// left that the step went to: the tree is not in key order, or its
    /// links lead back into it.
    OutOfOrder {
        /// The node the step came to.
        at: CellAddress,
        /// Whether the step went back, to the smaller keys.
        back: bool,
    },
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(error) => write!(f, "cursor: {error}"),
            Self::OtherReader {
                opened_at,
                opened_under,
                given_at,
                given_under,
            } => write!(
                f,
                "a cursor opened at {opened_at} under {opened_under} was given a reader at \
                 {given_at} under {given_under}"
            ),
            Self::OutOfOrder { at, back } => write!(
                f,
                "the key of the node at {at} is not {} the key the cursor stepped from",
                if *back { "before" } else { "after" }
            ),
            Self::TooDeep { at, limit } => write!(
                f,
                "the way down to the node at {at} is longer than {limit} nodes"
            ),
        }
    }
}

impl Error for CursorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Node(error) => Some(error),
            Self::OtherReader { .. } | Self::TooDeep { .. } | Self::OutOfOrder { .. } => None,
        }
    }
}

impl From<NodeError> for CursorError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}
