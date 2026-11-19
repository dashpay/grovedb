//! What one write does to the tree, as its caller states it.

use lignin_format::{tree_page::Hash, value_log::PendingValue, CellAddress};

/// A node that a link, a hosted-root pointer or a root pointer names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// A node that had a cell before the write, by the address the cell had
    /// then. It may be a node of the write.
    At(CellAddress),
    /// A node of the write, by its position in [`Write::nodes`]. This is the
    /// only name of a node the write creates.
    Node(usize),
    /// The node that the cell names there now: the child on the same side,
    /// the hosted root at the same position, the root the pointer named.
    ///
    /// The pointer of a host that is behind
    /// ([`Pages::host_is_behind`](super::Pages::host_is_behind)) keeps its
    /// bytes and stays behind: it is not followed.
    Kept,
}

/// A link of a written node: what Merk keeps about a child in its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkWrite<'a> {
    /// The child.
    pub child: Target,
    /// The child's hash.
    pub hash: &'a Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The length of the child's key.
    pub child_key_len: u8,
    /// The child's aggregate data (`tree-page.md` TP12).
    pub aggregate: &'a [u8],
}

/// The roots of the Merks a written node hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedWrite<'a> {
    /// The pointers the cell has. A node the write creates has none.
    Keep,
    /// The roots by position; `None` is a hosted Merk with no root.
    ///
    /// A host that is behind is written with every root named by its
    /// address or by its place in the write, by the write after which it is
    /// behind no more (`tree-layout.md` T3).
    Set(&'a [Option<Target>]),
}

/// The value of a written node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueWrite<'a> {
    /// What the cell holds: the value, or the pointer to it.
    Keep,
    /// A value that is stored in the cell.
    Inline(&'a [u8]),
    /// A value that the write's commit carries out of line: what stands in
    /// the cell until write-back has put the value into the value log.
    Pending(PendingValue),
}

/// One node of a write: what its cell holds when the write is applied.
///
/// Every part is stated, and the cell is changed where it differs: a hash
/// that changed is one hash record, and a value that is kept, or that has
/// the bytes the cell holds, is not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeWrite<'a> {
    /// The address of the node's cell before the write, or `None` for a
    /// node the write creates.
    pub at: Option<CellAddress>,
    /// The node's key. A node never changes its key.
    pub key: &'a [u8],
    /// The node's feature type with its aggregate data.
    pub feature: &'a [u8],
    /// The node's value hash.
    pub value_hash: &'a Hash,
    /// The node's key-value hash.
    pub kv_hash: &'a Hash,
    /// The link to the left child.
    pub left: Option<LinkWrite<'a>>,
    /// The link to the right child.
    pub right: Option<LinkWrite<'a>>,
    /// The roots of the Merks the node hosts.
    pub hosted: HostedWrite<'a>,
    /// The node's value.
    pub value: ValueWrite<'a>,
}

/// A pointer to the root of a Merk that no cell of the write holds: the
/// root Merk's, that of a Merk with no host, or that of a Merk whose host
/// the write does not hold. The write's caller keeps it, in the subtree
/// directory, and rewrites it from the write's report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootWrite {
    /// The root the pointer named before the write.
    pub was: Option<CellAddress>,
    /// The root it names now.
    pub now: Option<Target>,
    /// Whether the root stays in its page whatever the rules would do with
    /// it: the Merk has a host, whose cell the write does not hold, and the
    /// pointer in that cell is rewritten later (`tree-layout.md` T3). The
    /// directory entry names the root meanwhile.
    pub anchored: bool,
}

/// One write: the change sets of one operation, for all the Merks it wrote
/// (`tree-layout.md`, section 1 and P5).
///
/// The nodes may be given in any order. A node that another node of the
/// write points at is placed and carried with it. A node that nothing in the
/// write points at stays in its page, unless a root pointer of the write
/// names it (`tree-layout.md` T3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Write<'a> {
    /// The nodes whose cell is new or may differ.
    pub nodes: &'a [NodeWrite<'a>],
    /// The cells of the nodes the write deletes, by their address.
    pub removed: &'a [CellAddress],
    /// The roots of the Merks the write drops or clears, by their address
    /// (`tree-layout.md` D1, D4). No link or pointer names them afterwards.
    /// Their cells, and all that hangs below them, stay in their pages for
    /// the reclaimer.
    ///
    /// No host inside a dropped Merk is behind
    /// ([`Pages::host_is_behind`](super::Pages::host_is_behind)): its
    /// pointer would stay behind for good, in a cell that the reclaimer
    /// reads. The write that names the roots of such a host comes first,
    /// which costs one written page for each such host beyond what D1
    /// counts for a drop.
    pub dropped: &'a [CellAddress],
    /// The root pointers the write's caller holds.
    pub roots: &'a [RootWrite],
}
