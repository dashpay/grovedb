//! The bulk loader: the pages of Merk trees, built from their nodes.
//!
//! - [`Loader`] takes the nodes of a grove, each after its children
//!   ([`NodeLoad`]), and puts their cells into new pages: whole fragments
//!   of at most the largest size, in pages that are 90% full or more as
//!   long as there is a piece that fits ([`LoadParams`]), with the roots
//!   above them together in the pages above.
//! - [`InOrder`] takes the nodes of one Merk in the order of their keys, as
//!   a scan of its records gives them, and hands them to a loader.
//! - [`LoadValues`] is what a load tells about the values that are not in
//!   their cells, and [`LoadReport`] what it did.
//!
//! State sync restore, the migrator from RocksDB and the preload of a
//! benchmark all build trees whose shape is known before the first cell is
//! written. For them a load replaces one write for each node
//! ([`Fragments::apply`](crate::fragment::Fragments::apply)): no cell is
//! carried, no page is split and no value is appended twice.
//!
//! The shape of every tree is its writer's, as everywhere in this crate. A
//! load reads no key, and it changes nothing of what it is given: it
//! chooses the page and the slot of each cell, which is all a layout is
//! (`tree-layout.md` F5).
//!
//! # The stream
//!
//! A node is given after the nodes it points at: its two children, and the
//! roots of the Merks it hosts. [`Loader::node`] returns the subtree the
//! node is the root of ([`Loaded`]), and the node above names it by that.
//! So any order that gives a child before its parent will do: one Merk
//! after the other, or the Merk of a tree element between two nodes of the
//! Merk the element is in.
//!
//! The grove is laid out as one tree (`tree-layout.md` P2). A Merk that is
//! given to its host as a subtree of the load
//! ([`HostedLoad::Loaded`]) stays with the host as long as it fits there,
//! and [`HostedRoot`] says where its root went, for the subtree directory.
//! A Merk whose host is not in the load, or comes later, is placed with
//! [`Loader::root`], which returns the address and the root hash; the host
//! names it by the address ([`HostedLoad::At`]), or is written afterwards
//! like any other node.
//!
//! What a load keeps in memory is the nodes whose cells are in no page
//! yet: for each subtree that waits for its parent, at most a largest
//! fragment of cells. A value out of line is not among them: it goes to
//! [`LoadValues::hold`] when its node arrives.
//!
//! # Hashes
//!
//! **The rules are called by the loader, once for each hash, when a node
//! arrives.** Its children have arrived before it, so the hash of each is
//! known, and its own hash is made then and kept for the link of its
//! parent. A node costs one call of [`HashRules::kv_hash`], one of
//! [`HashRules::node_hash`] and, unless its value hash is given, one of
//! [`HashRules::value_hash`]. A cell goes into its page once, with every
//! hash in it: no hash is marked stale ([`Hashes`](crate::hash::Hashes)),
//! none is written as a record of its own, and no page is written for a
//! hash, or read for one but in the one case below.
//!
//! The other way is to insert cells without hashes, mark them and ask for
//! the root hash once. It makes the same hashes, and reads and writes
//! each cell's page once more for them, with one mark for each node in
//! memory and three hash records for each cell in the log.
//!
//! The price of the way taken is in what the rules are shown. A node is
//! hashed before its cell has a page, so the [`NodeRef`] a rule is given
//! names no cell: [`NodeRef::address`] and the addresses in its links and
//! hosted-root pointers are not the ones the cells get. Everything else
//! is the cell as it is stored. A rule makes a hash from keys, values,
//! features, aggregates and hashes, so this takes nothing from it.
//!
//! A value hash that no rule can make from the value and the hosted roots
//! is given with the node ([`NodeLoad::value_hash`]): a reference's, and
//! that of a host whose Merk comes later. A host that names a root by its
//! address and whose value hash is made has the root read for its hash,
//! one page step.
//!
//! # The layout
//!
//! A **piece** is the part of a subtree that is in no page yet: the root
//! and what hangs below it, as far as it was not cut off. A node that
//! arrives joins the pieces of its children to one.
//!
//! 1. **A piece above the largest fragment is cut below the node that
//!    arrived** ([`Layout::largest_fragment`](crate::fragment::Layout)).
//!    Every piece there of at least the small size goes into a page, as
//!    one fragment, and the node starts the piece above them, with what is
//!    small. So a fragment is what a subtree holds up to the largest size:
//!    of a tree that Merk keeps balanced, between a half and the whole of
//!    it. The nodes above the fragments of one level form the fragments of
//!    the next.
//! 2. **Pages are filled by level.** A piece that points at no cell of a
//!    page that is still filled goes to the page of level 0, and one that
//!    does to the level above that page. Each level fills one page at a
//!    time, with the pieces of every Merk of the load, in the order they
//!    are cut. So a cell never shares a page with the fragment below it,
//!    and the roots above the fragments of one level are together in the
//!    pages of the next. A node that joins pieces of different levels
//!    keeps the lower ones only if they are small: the others are cut, so
//!    that a piece of an upper level holds the nodes above and little
//!    else, and a path from a root passes about one page for each level.
//! 3. **A page that is not full enough is topped off.** When a piece does
//!    not fit the page of its level and the page is below the least fill,
//!    subtrees of that piece go into the page as fragments of their own,
//!    each time the largest that fits, until the page is full enough or
//!    nothing fits. Then the rest of the piece starts the next page. Of
//!    a piece that Merk balanced, the largest subtree that fits takes more
//!    than half the room, less one cell, so a few of them fill a page. A
//!    piece that is one cell has none, and the page is left as it is.
//!
//! What this leaves is what fragment maintenance accepts: every page
//! passes `TreePage::check_fragments`, no fragment is above the largest
//! size but a single cell that is, and a page holds only whole fragments.
//! A fragment that tops off a page can be below the small size; a later
//! write that holds it takes it to its parent's page when there is room
//! (M1).
//!
//! The levels are as many as cuts lie above each other: four for a million
//! nodes that Merk balanced, in pages of 32 KiB. A tree that is a chain
//! has one for every largest fragment of its length, and its pages are
//! valid and not full.
//!
//! # What the callers do
//!
//! - **New pages only.** A load writes the pages it starts
//!   ([`Pages::create`](crate::fragment::Pages::create)) and no other, and
//!   reads one only for the hash of a root that is named by its address.
//!   Nothing else writes those pages while it runs.
//! - **One step for a fragment.** A fragment goes into its page in one
//!   call of [`Pages::write`](crate::fragment::Pages::write), as one
//!   insert record for each cell. A page that the load has left
//!   ([`Loader::drain_pages`]) is not written again, so it can be written
//!   out while the load goes on.
//! - **Roots.** The address of every root goes into the subtree directory,
//!   and that of a root whose host was written before it into the host's
//!   cell, which is the caller's write.
//! - **An error ends the load.** The pages may hold a part of it, and the
//!   values a value that no cell holds: the caller rolls back the scope.
//!   Asking for a hash ([`Loader::hash_of`]) changes nothing and ends
//!   nothing.
//!
//! [`HashRules::kv_hash`]: crate::hash::HashRules::kv_hash
//! [`HashRules::node_hash`]: crate::hash::HashRules::node_hash
//! [`HashRules::value_hash`]: crate::hash::HashRules::value_hash
//! [`NodeRef`]: crate::node::NodeRef
//! [`NodeRef::address`]: crate::node::NodeRef::address

mod error;
mod load;
mod node;
mod order;

#[cfg(test)]
mod tests;

pub use error::{LoadError, ShapeError};
pub use load::{LoadCounters, LoadParams, LoadReport, Loader};
pub use node::{HostedLoad, HostedRoot, LinkLoad, LoadValues, Loaded, LoadedPage, NodeLoad, Root};
pub use order::InOrder;
