//! The error of a subtree directory operation.

use std::{error::Error, fmt};

use lignin_format::CellAddress;

use super::record::{MerkId, Prefix};
use crate::flat::FlatError;

/// Bytes that are not the record they were read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for Malformed {}

/// Why an operation on the subtree directory failed.
///
/// [`Self::Flat`] and [`Self::Damaged`] can leave a write half made: the
/// writer's pages are then no state of the directory, and whoever owns them
/// gives them up, as after a failed write of any flat keyspace
/// (`transactions.md` WT10). Every other error is a refusal, and a write
/// that is refused has changed no page, with the exceptions that
/// [`Directory::rekey`](super::Directory::rekey) and
/// [`Directory::rekey_pairs`](super::Directory::rekey_pairs) state.
#[derive(Debug)]
pub enum DirectoryError {
    /// The flat keyspace the directory is kept in failed.
    Flat(FlatError),
    /// The records are not a directory: one is not what its key says, or
    /// two that must agree do not.
    Damaged {
        /// The key of the record that was found wrong.
        key: Vec<u8>,
        /// What is wrong with it.
        what: &'static str,
    },
    /// A Merk was to be added under a prefix that has one.
    Exists {
        /// The prefix.
        prefix: Prefix,
    },
    /// No Merk is under the prefix: none was added, or it was dropped, or
    /// it is under a Merk that was.
    NotFound {
        /// The prefix.
        prefix: Prefix,
    },
    /// The Merk a host is said to be in is not in the directory.
    NoParent {
        /// The prefix of that Merk.
        parent: Prefix,
    },
    /// The host already has a Merk at the position.
    HostTaken {
        /// The prefix of the Merk the host is in.
        parent: Prefix,
        /// The position among the Merks of the host.
        position: u8,
    },
    /// A Merk was to move below itself.
    Inside {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// A Merk with no host was to be moved to another key of its host's
    /// Merk.
    NoHost {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// A Merk below a moved one is given no prefix by the move, and does
    /// not leave with it: it has a node, or a Merk below it is one that
    /// stays.
    Unnamed {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// A Merk with no root was dropped while Merks are still entered below
    /// it. A Merk that hosts another has the host's node, and so a root.
    NotEmpty {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// A root was said to be carried from an address at which no Merk has
    /// its root, and the same change names no Merk with the new address.
    UnknownRoot {
        /// The address the root was said to be at.
        address: CellAddress,
    },
    /// Two Merks would have their root in one cell.
    RootTaken {
        /// The address of the cell.
        address: CellAddress,
    },
    /// One change of roots names a Merk twice.
    Twice {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// A key or a tree type is longer than 255 bytes.
    TooLong {
        /// Which part is too long.
        what: &'static str,
        /// Its length in bytes.
        len: usize,
    },
    /// A Merk would be more than 65,535 Merks below one with no host.
    TooDeep,
    /// Every Merk id was given out.
    IdsExhausted,
    /// A left-over record of a Merk that is in use was to be taken out.
    InUse {
        /// The prefix of the Merk.
        prefix: Prefix,
    },
    /// What a dropped Merk left was said to be reclaimed while records of
    /// Merks below it are still there.
    Nested {
        /// The id of the Merk.
        id: MerkId,
    },
    /// No dropped Merk has the id.
    NotDropped {
        /// The id.
        id: MerkId,
    },
}

/// The first bytes of a prefix, which tell it from any other in a message.
struct Short<'a>(&'a Prefix);

impl fmt::Display for Short<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, ..] = self.0;
        write!(f, "{a:02x}{b:02x}{c:02x}{d:02x}..")
    }
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flat(error) => write!(f, "subtree directory: {error}"),
            Self::Damaged { key, what } => {
                write!(f, "subtree directory record ")?;
                key.iter().try_for_each(|byte| write!(f, "{byte:02x}"))?;
                write!(f, ": {what}")
            }
            Self::Exists { prefix } => {
                write!(f, "a Merk is entered under the prefix {}", Short(prefix))
            }
            Self::NotFound { prefix } => {
                write!(f, "no Merk is entered under the prefix {}", Short(prefix))
            }
            Self::NoParent { parent } => write!(
                f,
                "the host's Merk, under the prefix {}, is not entered",
                Short(parent)
            ),
            Self::HostTaken { parent, position } => write!(
                f,
                "the host in the Merk under the prefix {} has a Merk at position {position}",
                Short(parent)
            ),
            Self::Inside { prefix } => write!(
                f,
                "the Merk under the prefix {} would move below itself",
                Short(prefix)
            ),
            Self::NoHost { prefix } => write!(
                f,
                "the Merk under the prefix {} has no host, and is moved to no other",
                Short(prefix)
            ),
            Self::Unnamed { prefix } => write!(
                f,
                "the Merk under the prefix {} is below a moved one, and the move names no prefix \
                 for it",
                Short(prefix)
            ),
            Self::NotEmpty { prefix } => write!(
                f,
                "the Merk under the prefix {} has no root and Merks below it",
                Short(prefix)
            ),
            Self::UnknownRoot { address } => {
                write!(f, "no Merk has its root at {address}")
            }
            Self::RootTaken { address } => {
                write!(f, "two Merks would have their root at {address}")
            }
            Self::Twice { prefix } => write!(
                f,
                "the Merk under the prefix {} is named twice in one change",
                Short(prefix)
            ),
            Self::TooLong { what, len } => {
                write!(f, "{what} of {len} bytes is longer than 255 bytes")
            }
            Self::TooDeep => write!(f, "a Merk is more than 65535 Merks deep"),
            Self::IdsExhausted => write!(f, "every Merk id was given out"),
            Self::InUse { prefix } => write!(
                f,
                "the Merk under the prefix {} is in use, and no left-over",
                Short(prefix)
            ),
            Self::Nested { id } => write!(
                f,
                "records of Merks below the dropped Merk {id} are still there"
            ),
            Self::NotDropped { id } => write!(f, "no dropped Merk has the id {id}"),
        }
    }
}

impl Error for DirectoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Flat(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FlatError> for DirectoryError {
    fn from(error: FlatError) -> Self {
        Self::Flat(error)
    }
}
