//! The error of an evaluation.

use std::{error::Error, fmt};

use lignin_format::{tree_page::HashField, CellAddress};

use super::{rules::Refusal, stale::name};
use crate::fragment::FragmentError;

/// Why a hash was not evaluated.
///
/// Whatever the error, every hash that is stale is still marked: a node's
/// marks are taken off only once all of its hashes are in its cell. What
/// was evaluated before the error stays evaluated.
#[derive(Debug)]
pub enum HashError {
    /// A page was not read or not changed. It is the error of whoever
    /// implements [`Pages`](crate::fragment::Pages).
    Pages(FragmentError),
    /// The value of a node, which is not in its cell, was not read. It is
    /// the error of the [`ValueReader`](super::ValueReader).
    Value(Refusal),
    /// The reader gave a value of another length than the cell states.
    ValueLen {
        /// The node.
        node: CellAddress,
        /// The length the cell states.
        len: u32,
        /// The bytes that were read.
        read: usize,
    },
    /// A rule refused to make a hash. It is the error of the
    /// [`HashRules`](super::HashRules).
    Rule(Refusal),
    /// A mark, a link or a hosted-root pointer names a slot that holds no
    /// cell.
    NoNode {
        /// The address.
        address: CellAddress,
    },
    /// A mark names a hash the cell does not store: the hash of a child on
    /// a side where the node has none.
    NoHash {
        /// The node.
        node: CellAddress,
        /// The hash that is marked.
        field: HashField,
    },
    /// The value hash of a host is marked while the host is behind
    /// ([`Pages::host_is_behind`](crate::fragment::Pages::host_is_behind)):
    /// its pointers are not followed and its element is not written yet, so
    /// the hash waits for the write that names the host's roots again
    /// (`transactions.md` SC9).
    HostBehind {
        /// The host.
        host: CellAddress,
    },
    /// The marked nodes point at each other in a circle.
    Cycle {
        /// A node of the circle.
        node: CellAddress,
    },
    /// A cell does not point at what it pointed at when it was read, a
    /// step earlier in the same evaluation: the child on a marked side, a
    /// root it hosts, or the place of its value. Something else changed the
    /// pages meanwhile, and a hash made from what was read would be wrong.
    Changed {
        /// The node.
        node: CellAddress,
    },
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pages(error) => write!(f, "pages: {error}"),
            Self::Value(error) => write!(f, "value: {error}"),
            Self::ValueLen { node, len, read } => {
                write!(f, "the value of {node} is {len} bytes and {read} were read")
            }
            Self::Rule(error) => write!(f, "hash rule: {error}"),
            Self::NoNode { address } => write!(f, "no node at {address}"),
            Self::NoHash { node, field } => {
                write!(f, "the node at {node} stores no {}", name(*field))
            }
            Self::HostBehind { host } => {
                write!(f, "the host at {host} is behind the roots it hosts")
            }
            Self::Cycle { node } => {
                write!(f, "the marked nodes point in a circle through {node}")
            }
            Self::Changed { node } => {
                write!(f, "the cell at {node} changed during the evaluation")
            }
        }
    }
}

impl Error for HashError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pages(error) => Some(error),
            Self::Value(error) | Self::Rule(error) => Some(error.as_ref()),
            Self::ValueLen { .. }
            | Self::NoNode { .. }
            | Self::NoHash { .. }
            | Self::HostBehind { .. }
            | Self::Cycle { .. }
            | Self::Changed { .. } => None,
        }
    }
}
