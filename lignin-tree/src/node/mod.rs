//! Node handles: the nodes of Merk trees as views into frames.
//!
//! - [`NodeRef`] is one node, borrowed from the frame that holds its page:
//!   the key, the value or the pointer to it, the feature type, the hashes,
//!   the links and the hosted roots. [`NodePage`] is the page it is read
//!   from.
//! - [`NodeMut`] is one node of a page that is open for changes
//!   ([`NodePageMut`]): set the value, set a link, and remove the node.
//!   [`NodePageMut::insert`] puts a node into the page.
//! - [`NodeHandle`] is the name of a node that outlives its view, and
//!   [`Lease`] is what it is good under.
//! - [`NodeReader`] reads committed pages through the store and hands out
//!   [`PinnedPage`] and [`PinnedNode`], which keep their frame pinned.
//!
//! The layout of the nodes is `docs/lignindb/tree-layout.md` and the bytes of
//! a page are `docs/lignindb/tree-page.md`. This module adds no format. It
//! knows no Merk rule: a feature type, an aggregate and a hash are bytes to
//! it, as they are to the page.
//!
//! ```
//! use lignin_format::{
//!     tree_page::{CellContent, CellValue},
//!     PageId, PageSize,
//! };
//! use lignin_tree::node::{Lease, NodePage, NodePageMut};
//!
//! # fn main() -> Result<(), lignin_tree::node::NodeError> {
//! let size = PageSize::DEFAULT;
//! let lease = Lease::take();
//! let leaf = |key, value| CellContent {
//!     key,
//!     feature: &[0],
//!     value_hash: &[1; 32],
//!     kv_hash: &[2; 32],
//!     left: None,
//!     right: None,
//!     hosted_roots: &[],
//!     value: CellValue::Inline(value),
//!     fragment_root: true,
//! };
//!
//! // A write transaction changes its private copy of a page.
//! let mut frame = vec![0; size.get()];
//! let mut page = NodePageMut::create(&mut frame, size, PageId::MIN, lease, ())?;
//! let apple = page.insert(&leaf(b"apple", b"red"))?;
//! let pear = page.insert(&leaf(b"pear", b"green"))?;
//!
//! // A handle is followed for as long as no cell has left its page.
//! let mut node = page.resolve_mut(apple).expect("no cell has left the page");
//! node.set_value(CellValue::Inline(b"green"))?;
//! assert_eq!(node.node().value(), CellValue::Inline(b"green"));
//!
//! // Removing a node ends every handle into its page. The node that stays
//! // is read again by its address, which gives its new handle.
//! page.resolve_mut(apple).expect("still good").remove()?;
//! let nodes = NodePage::open(&frame, lease)?;
//! assert!(nodes.resolve(pear).is_none());
//! let node = nodes.node(pear.slot()).expect("the pear stays in its slot");
//! assert_eq!(node.key(), b"pear");
//! assert_eq!(nodes.resolve(node.handle()), Some(node));
//! # Ok(())
//! # }
//! ```
//!
//! # Reading
//!
//! A node is read in three steps:
//!
//! 1. The frame. [`NodeReader::pin`] takes the page version from the buffer
//!    pool, or loads it (`lignin_store::load`). A write transaction reads
//!    its private copy under the copy's latch instead.
//! 2. [`NodePage::open`] reads the page header.
//! 3. [`NodePage::node`] reads the slot, the cell's header and the fixed
//!    part of its links. The key, the feature and the value are slices of
//!    the frame, and the value's bytes are not touched unless they are
//!    asked for.
//!
//! **Reading a node allocates nothing.** Steps 2 and 3 never allocate,
//! whatever the page holds, and neither does following a handle. Step 1
//! allocates nothing in this module. What the store allocates to get the
//! frame is the store's to say, and comes to this:
//!
//! - nothing when the page is in the pool
//! - nothing for a load once the caller's [`LoadBuffers`] have grown to
//!   what the page's blocks need, unless a record of the page's chain makes
//!   the page codec lay a cell out again or gather the page's unused bytes:
//!   a cell that changes its length, or one that arrives where the end of
//!   the page has no room for it
//! - one frame beside the pool's fixed ones for a page that is not in the
//!   pool when no frame can be evicted, because every one is pinned,
//!   private or not yet written back. A reader that keeps pages pinned, as
//!   a cursor keeps its parent stack, has to count them into its pool
//!
//! **Bytes behind a view do not change** (`transactions.md` LT2). A view
//! borrows from what keeps its frame still: a [`PinnedPage`], whose page
//! version never changes (PV2), or the latch of a private copy. The
//! borrow is the proof, so a view cannot be held across a change to its
//! frame.
//!
//! **A view lives inside its operation's guard** (PN5). For committed
//! pages the types see to it: [`NodeReader`] borrows the guard, and a
//! [`PinnedPage`] or [`PinnedNode`] borrows it through the reader. The
//! guard is [`OperationGuard`], which whoever keeps the guards implements
//! (TXN-02). A view that is opened on bytes directly ([`NodePage::open`],
//! [`NodePageMut::open`]), as on the latch of a private copy, is bound to
//! nothing but those bytes: its caller holds the operation's guard for as
//! long as it uses the view or an address it read through it, and gives the
//! latch back within the physical step (LT3).
//!
//! # Addresses and handles
//!
//! An address that a page stores is exact (`tree-layout.md` S3): the link of
//! a parent, a hosted-root pointer, a directory entry. It is followed with
//! [`NodeReader::node`] or [`NodePage::node`], and a slot that holds no cell
//! is an error.
//!
//! An address kept in memory is a hint (S6). It is a [`NodeHandle`]: the
//! address, the page's count of departures when it was read, and the lease
//! it was read under. [`NodePage::resolve`] follows it only if all three
//! still fit, and otherwise the holder finds the node by its key, from the
//! root of its Merk. Comparing the key of the cell that is found is not a
//! check: a slot can be given to a cell with the same key.
//!
//! What ends a handle, and what does not:
//!
//! | Since the handle was issued | The handle |
//! | --- | --- |
//! | a hash, a link, a value or the feature of any cell of the page changed | is good |
//! | a cell arrived in the page | is good |
//! | the page got a new version by a commit that took no cell out of it | is good |
//! | a value pointer in the page was rewritten, or a pending value placed | is good |
//! | its cell was deleted, or carried to another page | ends: the count moved |
//! | another cell left the page, by a write or by the removal of what a dropped subtree left | ends: the count moved. The node is still in its slot, and a view of it gives a new handle |
//! | the page was freed | ends: there is no page, or another one (see below) |
//! | the lease ended | ends |
//!
//! **What the count cannot tell, and the lease does.** The count is the
//! page's own, and it tells the states of a page apart along one line of
//! descent only. Three histories leave that line:
//!
//! - *Page id reuse.* A page that is created under a reused id starts again
//!   at 0, so a handle that outlived its page could meet its own count in a
//!   page it was never read from.
//! - *A frame that goes back.* A frame that a rollback puts back to an
//!   earlier state counts on from that state: the count a handle was issued
//!   at can come again with another cell in the slot.
//! - *Two copies side by side.* A cell that arrives does not move the
//!   count. Two private copies of one page, or a private copy and a later
//!   committed version, can each put a cell into the same free slot at the
//!   same count.
//!
//! None of them is left to the count. A handle is good only under its
//! [`Lease`], and a lease is given up before any of the three can be seen:
//! its holder keeps pinned the oldest version it reads at, which holds back
//! the reuse of every page id it can see (`transactions.md` ID2), takes a
//! new lease when a frame goes back, and gives each of two copies a lease
//! of its own. So no handle outlives the write that could free its page,
//! and a handle that is kept past its lease is refused, not followed.
//!
//! # Changing
//!
//! [`NodePageMut`] opens the private copy of a page under its exclusive
//! latch. Each change is one call of the page codec (`tree-page.md`,
//! section 5) and one record of `page-delta.md`:
//!
//! | Change | Call | Ends the handles into the page |
//! | --- | --- | --- |
//! | a new node | [`NodePageMut::insert`], [`NodePageMut::insert_cell`] for a carried cell | no |
//! | the value | [`NodeMut::set_value`] | no |
//! | a link | [`NodeMut::set_link`], [`NodeMut::set_child`] for its address alone | no |
//! | a hash | [`NodeMut::set_hash`] | no |
//! | the feature, the fragment-root mark, the hosted roots | [`NodeMut::set_feature`], [`NodeMut::set_fragment_root`], [`NodeMut::set_hosted_root`], [`NodeMut::set_hosted_roots`] | no |
//! | the whole cell, for the same node | [`NodeMut::replace`] | no |
//! | the node leaves | [`NodeMut::remove`] | yes |
//!
//! The steps of carrying a cell that are made in pages are
//! [`NodePageMut::insert_cell`] in the page it goes to,
//! [`NodeMut::remove`] in the page it leaves, and the rewriting of the
//! address in the cell that points at it ([`NodeMut::set_child`],
//! [`NodeMut::set_hosted_root`]). A carry is more than those: the root of
//! a Merk also has its directory entry rewritten (`tree-layout.md` S3,
//! TREE-05), and a value out of line is appended again with the new page
//! as its owner (X1, TREE-08). Which cells are carried, and when, is
//! TREE-03's.
//!
//! Every change that a page took is told to the page's [`ChangeSink`], as
//! the record that makes it. The records of one page, in order, are what a
//! run of that page holds.
//!
//! # What the callers do
//!
//! - **Every change to a tree page goes through [`NodePageMut`]**, or
//!   through the page codec with the same care: a slot is given to another
//!   node only after a removal (`tree-page.md` W1).
//! - **A holder of handles keeps its lease to one line of descent**
//!   ([`Lease`]). It keeps pinned the oldest version it reads at, or reads
//!   at the latest version only and takes a new lease when that has moved,
//!   as a write transaction that has written nothing does. It takes a new
//!   lease when a frame it reads is put back: a rollback, a rollback to a
//!   savepoint, a scope rollback. It gives each write scope a lease of its
//!   own, and takes a new one when it rebases or reconciles a merged scope
//!   (`transactions.md` PN8).
//! - **Background work takes a cell out of the current version of a page
//!   only**, as the pool's page step does (PV7). A version that is no
//!   longer current keeps its cells and its count.
//! - **A reader holds an operation guard** for as long as it uses what it
//!   read, taken before it reads the latest version (PN5).
//! - **A writer holds the exclusive latch** of the copy it changes for one
//!   physical step, and no view into the copy outlives the step (LT3).
//!
//! [`LoadBuffers`]: lignin_store::load::LoadBuffers

mod error;
mod handle;
mod read;
mod view;
mod write;

#[cfg(test)]
mod tests;

pub use error::NodeError;
pub use handle::{Lease, NodeHandle};
pub use read::{NodeReader, OperationGuard, PinnedNode, PinnedPage};
pub use view::{NodePage, NodeRef, Nodes};
pub use write::{ChangeSink, NodeMut, NodePageMut};
