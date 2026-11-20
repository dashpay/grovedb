//! What a load is given, and what it gives back.

use std::fmt;

use lignin_format::{
    tree_page::Hash,
    value_log::{PendingValue, ValuePlace},
    CellAddress, PageId,
};

use crate::{
    fragment::{ValueWrite, Values},
    hash::Refusal,
    node::Lease,
    value::TreeValues,
};

/// A subtree that a load has taken: its root, and everything below it.
///
/// It is given to the node that is its parent or its host, or to
/// [`Loader::root`](super::Loader::root), once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Loaded {
    pub(super) load: Lease,
    pub(super) index: u32,
    pub(super) serial: u64,
}

impl fmt::Display for Loaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "subtree {}", self.serial)
    }
}

/// A link of a loaded node: what Merk keeps about a child in its parent,
/// but for the child's hash, which the rules make, and the length of its
/// key, which the load knows.
///
/// `C` names the child: a [`Loaded`] subtree for [`Loader::node`], and
/// nothing for a stream in key order, where the child is found by its place
/// ([`InOrder`](super::InOrder)).
///
/// [`Loader::node`]: super::Loader::node
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkLoad<'a, C = Loaded> {
    /// The child.
    pub child: C,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The child's aggregate data (`tree-page.md` TP12).
    pub aggregate: &'a [u8],
}

/// The root of a Merk that a loaded node hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedLoad {
    /// A hosted Merk with no root.
    Empty,
    /// A subtree of this load. It is laid out with its host, as a child of
    /// it (`tree-layout.md` P2), and the load reports where its root went
    /// ([`HostedRoot`]).
    Loaded(Loaded),
    /// A cell that is in a page: a root that an earlier load or this one
    /// placed ([`Loader::root`](super::Loader::root)). It is marked as the
    /// root of its fragment, and stays where it is.
    At(CellAddress),
}

/// One node of a load: what its cell holds, but for the addresses and the
/// hashes the load makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeLoad<'a, C = Loaded> {
    /// The node's key.
    pub key: &'a [u8],
    /// The node's feature type with its aggregate data.
    pub feature: &'a [u8],
    /// The node's value.
    pub value: &'a [u8],
    /// The node's value hash, or `None` for one the rules make from the
    /// value and the roots of the Merks the node hosts. A hash that no rule
    /// can make from those, a reference's, is given.
    pub value_hash: Option<&'a Hash>,
    /// The link to the left child.
    pub left: Option<LinkLoad<'a, C>>,
    /// The link to the right child.
    pub right: Option<LinkLoad<'a, C>>,
    /// The roots of the Merks the node hosts, by position.
    pub hosted: &'a [HostedLoad],
}

/// A root that a load placed: a node that no cell of the load points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// Where the root's cell is: the address for the subtree directory, or
    /// for the pointer of a host that is written later.
    pub address: CellAddress,
    /// The root's hash: the root hash of its Merk.
    pub hash: Hash,
}

/// Where the root of a Merk went that was given to its host as a subtree
/// of the load ([`HostedLoad::Loaded`]): the address for the Merk's entry
/// in the subtree directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostedRoot {
    /// The subtree, as the host named it.
    pub subtree: Loaded,
    /// Where the root's cell is.
    pub address: CellAddress,
}

/// A page that a load started and will not write again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedPage {
    /// The page.
    pub page: PageId,
    /// The bytes its cells and their slots take: the hint of the page
    /// ([`Hints::note`](crate::fragment::Hints::note)).
    pub used: usize,
}

/// The values of the cells a load builds.
///
/// A load gives every value to [`Self::hold`] when its node arrives, and
/// asks [`Self::place`] what the cell of a value out of line holds when
/// the node's cell goes into its page, which by default says with
/// [`Values::placed`] which page owns the value. It calls nothing else of
/// [`Values`].
pub trait LoadValues: Values {
    /// Takes the value of a node that is being loaded. Returns `None` for
    /// a value that is stored in its cell, and for one out of line what
    /// stands in the cell until the value has its place.
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal>;

    /// Says what the cell of the value `pending`, which [`Self::hold`]
    /// took, is written with when it goes into `page`. A load calls this
    /// once for each value out of line, right before it writes the
    /// fragment the cell is in, and writes the cell with what it returns.
    ///
    /// The default leaves the pending value in the cell and tells
    /// [`Values::placed`], so the value waits for its commit as that of any
    /// write does. A load that puts its values into the value log as their
    /// cells get their pages appends the value for `page` here and returns
    /// the pointer: the cell holds it from its first write on. A place of
    /// another length than the value is refused by the page.
    fn place(&mut self, pending: PendingValue, page: PageId) -> Result<ValuePlace, Refusal> {
        self.placed(pending, page);
        Ok(ValuePlace::Pending(pending))
    }
}

impl LoadValues for TreeValues {
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal> {
        match self.write(value)? {
            ValueWrite::Pending(pending) => Ok(Some(pending)),
            ValueWrite::Inline(_) | ValueWrite::Keep => Ok(None),
        }
    }
}
