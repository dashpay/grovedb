//! The ordered cursor: the nodes of one Merk, in key order.
//!
//! [`Cursor`] stands on one node of a tree that is hosted in tree pages
//! (`docs/lignindb/tree-layout.md`) and goes to the first node at or after
//! a key ([`Cursor::seek`]), to the last one at or before it
//! ([`Cursor::seek_for_prev`]), to either end, and from a node to the one
//! beside it in key order ([`Cursor::next`], [`Cursor::prev`]). It reads
//! the committed pages of one version, through
//! [`NodeReader`], and never sees another.
//!
//! The pages have no sibling links and no parent pointers (C5). So a cursor
//! remembers the way it came: the nodes above the one it stands on, from
//! the root down, each with the side on which the way went on. A step goes
//! down through the links of the node it stands on, which are exact
//! addresses, or up through that way. It never starts again at the root.
//!
//! # Opening a cursor, and what it is given
//!
//! A cursor is opened on a reader and the root of a Merk
//! ([`Cursor::open`]). From the reader it takes the version it reads at
//! and the lease its handles are issued under. Every call after that is one
//! operation (`transactions.md`, section 1) and is given the reader of that
//! operation: the same version and the same lease, under the operation's
//! own guard. A reader of another version or lease is refused
//! ([`CursorError::OtherReader`]), and the cursor stays as it was.
//!
//! | The cursor is given | By | For |
//! | --- | --- | --- |
//! | the root of the Merk, or `None` for a Merk with no node | [`Cursor::open`], [`Cursor::retarget`] | the whole walk: the address the directory entry or the host's cell stores at the cursor's version |
//! | the version and the lease | the reader of [`Cursor::open`] | its life |
//! | a guard, and the pages | the reader of each call | that call, and the nodes it returns |
//!
//! [`Cursor::retarget`] points the cursor at another Merk of the same
//! version, which is how one cursor reads an element and then the tree
//! under it in one state (`engine.md`, `retarget_cursor`).
//!
//! # What the caller keeps
//!
//! - **The version stays pinned for as long as the cursor is used** (PN1,
//!   PN8). The cursor cannot see a pin, so this is the caller's: a
//!   snapshot's pin for a cursor of a snapshot, and a pin of the cursor's
//!   own, taken when it is opened, for a cursor of a write transaction
//!   (RD6). The pin is also what the lease needs (rule 1 of
//!   [`Lease`]): the cursor reads at one version only,
//!   so its lease is on one line of descent.
//! - **One store.** The readers of all calls read the pages of the store
//!   the cursor was opened on. A lease belongs to one holder, so a reader
//!   with the cursor's lease is that holder's.
//! - **The root is the one the version has.** It is read from the
//!   directory or from the host's cell at the cursor's version, by the
//!   rules of those (TREE-05, `tree-layout.md` C3).
//!
//! With these, the cursor is stable under the snapshot it was opened on
//! (RD6): a published page version never changes what its cells hold
//! (PV2), and no commit that comes later is visible at the cursor's
//! version.
//!
//! # Pins
//!
//! **A cursor keeps no page pinned.** Its way is kept as
//! [`NodeHandle`]s, 24 bytes each, and not as
//! pinned frames:
//!
//! - between two calls it holds no pin at all. A pin borrows the guard of
//!   its operation (PN5), and the guard ends with the call
//! - inside a call it holds one page at a time: the page it is reading. It
//!   lets that page go before it asks for the next
//! - a node that a call returns keeps its page pinned until the caller
//!   drops it. Those pins are the caller's
//!
//! So a cursor adds nothing to the size its pool has to have, whatever the
//! height of its tree, and a scan runs in a pool of one frame. The price is
//! paid in lookups: every page a step reads is asked of the pool again.
//! That is one lookup for the page the cursor stands on, in every step, and
//! one for each link the step follows into another page, down or up.
//! Consecutive keys are mostly in one page (V4), so a scan of a tree as the
//! layout rules cluster it asks about once for each node. With every node
//! in a page of its own it asks at most three times for each node. A page
//! that left the pool between two steps is loaded again, which a cursor
//! that kept it pinned would not pay.
//!
//! A call allocates nothing once the cursor's way and its key have grown
//! to what the tree needs: the way to the height of the tree, the key to
//! the longest key. What the store allocates for a page is the store's
//! (see [`crate::node`]).
//!
//! # When a handle has ended
//!
//! A handle on the cursor's way is a hint (S6), and it is checked whenever
//! it is followed. At a pinned version one thing ends it: a step of
//! background work that takes what a dropped subtree left out of the
//! current version of a page (PV7), when that page is on the cursor's way.
//! The node the cursor stands on is still in its Merk. The cursor then
//! finds its key again from the root, which gives it a new way, and goes
//! on. [`CursorStats::refinds`] counts these. They are the only walks from
//! the root that a step makes.
//!
//! The same rule says what a step does if the key is not found again: it
//! goes to the first node after the key, or with [`Cursor::prev`] to the
//! last one before it. That cannot happen while the caller keeps what is
//! listed above.
//!
//! # What a cursor trusts, and what it checks
//!
//! - **The order of the tree.** A cursor compares keys as byte strings on
//!   the way down from the root, and takes the tree to be ordered that
//!   way. Whether it is, is the physical invariant checker's to say
//!   (VER-07).
//! - **Nothing about placement** (F5). Two nodes may share a page or not,
//!   and a page may hold nodes of other Merks, which a cursor never reads:
//!   it follows the links of its own tree and no hosted-root pointer.
//!
//! Two things it does check, so that every call returns and every scan
//! ends, whatever the links of the pages are:
//!
//! - **A way down has an end.** A way of more than [`Cursor::MAX_DEPTH`]
//!   nodes is longer than any tree's, and is refused
//!   ([`CursorError::TooDeep`]): links that lead back into the tree.
//! - **A step goes on.** The node a step comes to has a greater key than
//!   the node it left, or with [`Cursor::prev`] a smaller one. A node that
//!   has not is refused ([`CursorError::OutOfOrder`]). So the keys of a
//!   scan only rise, or only fall, and a scan of a tree of n nodes ends
//!   within n steps or with an error.
//!
//! # What is not here
//!
//! - **The writes of a transaction.** A cursor reads committed pages. The
//!   cursor of a write transaction merges two sides in key order (RD6): the
//!   committed side at the version that was the latest when it was opened,
//!   which is this cursor, and the transaction's own writes, which are
//!   TXN-03's.
//! - **Values out of line.** A cursor returns the node. Reading a value
//!   that is not in its cell is TREE-08's.
//! - **Records.** The node record Merk's callers expect from a cursor is
//!   built from the node and its children's cells by MERK-05
//!   (`tree-page.md`, section 7).

mod error;

#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use lignin_format::{tree_page::Side, CellAddress, CommitVersion, PageId};
use lignin_store::load::LoadBuffers;

pub use self::error::CursorError;
use crate::node::{
    Lease, NodeError, NodeHandle, NodeReader, OperationGuard, PinnedNode, PinnedPage,
};

/// One node above the node a cursor stands on, and the side of it on which
/// the way goes on.
#[derive(Debug, Clone, Copy)]
struct Above {
    node: NodeHandle,
    side: Side,
}

/// What a cursor has done since it was opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorStats {
    /// Walks down from the root: one for each seek, and one for each time
    /// the cursor found its key again.
    pub descents: u64,
    /// Times the cursor found its key again from the root, because a
    /// handle on its way had ended.
    pub refinds: u64,
    /// Pages the cursor asked its reader for: each is one lookup in the
    /// buffer pool, and a load if the page is not there.
    pub lookups: u64,
}

/// Where one walk down ended.
struct Down<'r> {
    /// The last page that was read, still pinned.
    held: PinnedPage<'r>,
    /// The node with the key that was looked for, if the walk met it.
    found: Option<NodeHandle>,
}

/// What one move came to.
enum Moved<'r> {
    /// The cursor stands on this node.
    To(PinnedNode<'r>),
    /// There is no node on that side.
    End,
    /// A handle on the way had ended. The cursor's key is as it was before
    /// the move, which is made again from the root.
    Lost,
}

impl<'r> Moved<'r> {
    /// The node the move ended at, or that it ended at none. `None` for a
    /// move that has to be made again.
    fn settled(self) -> Option<Option<PinnedNode<'r>>> {
        match self {
            Self::To(node) => Some(Some(node)),
            Self::End => Some(None),
            Self::Lost => None,
        }
    }
}

/// The side a move goes to, and the side it comes from: with `back`, to the
/// smaller keys.
const fn sides(back: bool) -> (Side, Side) {
    if back {
        (Side::Left, Side::Right)
    } else {
        (Side::Right, Side::Left)
    }
}

/// A cursor over the nodes of one Merk, in key order, at one version. See
/// the [module documentation](self).
///
/// It is not positioned until the first seek. Stepping past either end
/// leaves it not positioned again, and so does a call that fails with any
/// error but [`CursorError::OtherReader`].
#[derive(Debug, Clone)]
pub struct Cursor {
    root: Option<CellAddress>,
    at: CommitVersion,
    lease: Lease,
    /// The nodes above the one the cursor stands on, from the root down.
    way: Vec<Above>,
    here: Option<NodeHandle>,
    key: Vec<u8>,
    /// The room of the key a re-find looks for, kept from one to the next.
    sought: Vec<u8>,
    stats: CursorStats,
}

impl Cursor {
    /// The most nodes on one way down. A link holds the heights of the
    /// child's two subtrees in one byte each (`tree-page.md`), so a child is
    /// at most 256 nodes high and the node above it 257: no way from a root
    /// to a node is longer.
    pub const MAX_DEPTH: usize = u8::MAX as usize + 2;

    /// A cursor over the Merk whose root is at `root`, at the version of
    /// `reader` and under its lease. `None` is a Merk with no node.
    ///
    /// `root` is the address that the version stores for the root: in the
    /// Merk's directory entry, or in the cell of its host. The cursor is
    /// not positioned.
    pub fn open<G: OperationGuard + ?Sized>(
        reader: &NodeReader<'_, G>,
        root: Option<CellAddress>,
    ) -> Self {
        Self {
            root,
            at: reader.at(),
            lease: reader.lease(),
            way: Vec::new(),
            here: None,
            key: Vec::new(),
            sought: Vec::new(),
            stats: CursorStats::default(),
        }
    }

    /// Points the cursor at the Merk whose root is at `root`, at the same
    /// version and under the same lease. The cursor is not positioned.
    pub fn retarget(&mut self, root: Option<CellAddress>) {
        self.leave();
        self.root = root;
    }

    /// The root of the Merk the cursor walks.
    pub const fn root(&self) -> Option<CellAddress> {
        self.root
    }

    /// The version the cursor reads at.
    pub const fn at(&self) -> CommitVersion {
        self.at
    }

    /// The lease the cursor's handles are issued under.
    pub const fn lease(&self) -> Lease {
        self.lease
    }

    /// Whether the cursor stands on a node.
    pub const fn valid(&self) -> bool {
        self.here.is_some()
    }

    /// The key of the node the cursor stands on.
    pub fn key(&self) -> Option<&[u8]> {
        self.here.map(|_| self.key.as_slice())
    }

    /// The name of the node the cursor stands on, as it was when the
    /// cursor came to it: a hint, like every handle.
    pub const fn handle(&self) -> Option<NodeHandle> {
        self.here
    }

    /// How many nodes are above the one the cursor stands on.
    pub fn depth(&self) -> Option<usize> {
        self.here.map(|_| self.way.len())
    }

    /// What the cursor has done since it was opened.
    pub const fn stats(&self) -> CursorStats {
        self.stats
    }

    /// Goes to the first node whose key is `key` or greater, and returns
    /// it. `None` if there is none: the cursor is then not positioned.
    pub fn seek<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
        key: &[u8],
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.find(Some(key), false);
        self.settle(moved)
    }

    /// Goes to the last node whose key is `key` or less, and returns it.
    /// `None` if there is none: the cursor is then not positioned.
    pub fn seek_for_prev<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
        key: &[u8],
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.find(Some(key), true);
        self.settle(moved)
    }

    /// Goes to the node with the smallest key, and returns it. `None` for
    /// a Merk with no node.
    pub fn seek_to_first<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.find(None, false);
        self.settle(moved)
    }

    /// Goes to the node with the greatest key, and returns it. `None` for
    /// a Merk with no node.
    pub fn seek_to_last<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.find(None, true);
        self.settle(moved)
    }

    /// Goes to the next node in key order, and returns it. `None` after
    /// the last node, and for a cursor that is not positioned, which stays
    /// so.
    pub fn next<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.step(false);
        self.settle(moved)
    }

    /// Goes to the node before this one in key order, and returns it.
    /// `None` before the first node, and for a cursor that is not
    /// positioned, which stays so.
    pub fn prev<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let moved = self.walk(reader, buffers)?.step(true);
        self.settle(moved)
    }

    /// The node the cursor stands on, pinned for the operation of
    /// `reader`: for a caller that reads it in a later operation than the
    /// one that moved the cursor. `None` for a cursor that is not
    /// positioned.
    ///
    /// If the cursor's handle has ended, the node is found again by its
    /// key. If no node has that key, which cannot be while the cursor's
    /// version is pinned, the answer is `None` and the cursor is not
    /// positioned.
    pub fn node<'r, G: OperationGuard + ?Sized>(
        &mut self,
        reader: &NodeReader<'r, G>,
        buffers: &mut LoadBuffers,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let found = self.walk(reader, buffers)?.read();
        self.settle(found)
    }

    /// Begins one call: the cursor, with the reader of the call's
    /// operation.
    fn walk<'c, 'r, G: OperationGuard + ?Sized>(
        &'c mut self,
        reader: &'c NodeReader<'r, G>,
        buffers: &'c mut LoadBuffers,
    ) -> Result<Walk<'c, 'r, G>, CursorError> {
        if reader.at() != self.at || reader.lease() != self.lease {
            return Err(CursorError::OtherReader {
                opened_at: self.at,
                opened_under: self.lease,
                given_at: reader.at(),
                given_under: reader.lease(),
            });
        }
        Ok(Walk {
            cursor: self,
            reader,
            buffers,
        })
    }

    /// Ends one call: a call that came to no node leaves no position.
    fn settle<'r>(
        &mut self,
        moved: Result<Option<PinnedNode<'r>>, CursorError>,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        if !matches!(moved, Ok(Some(_))) {
            self.leave();
        }
        moved
    }

    /// Leaves the position: the cursor stands nowhere.
    fn leave(&mut self) {
        self.way.clear();
        self.here = None;
    }

    /// Takes the key of the position out of the cursor, to look for it:
    /// the cursor's key is then the room the last such key had.
    fn take_key(&mut self) -> Vec<u8> {
        let room = std::mem::take(&mut self.sought);
        std::mem::replace(&mut self.key, room)
    }
}

/// A cursor in one call, with the reader and the buffers of the call.
struct Walk<'c, 'r, G: ?Sized> {
    cursor: &'c mut Cursor,
    reader: &'c NodeReader<'r, G>,
    buffers: &'c mut LoadBuffers,
}

impl<'r, G: OperationGuard + ?Sized> Walk<'_, 'r, G> {
    /// Goes to the first node at or after `key`, or with `back` to the
    /// last one at or before it. With no key, to the first node, or the
    /// last.
    ///
    /// A walk down is made again if a page step took a cell out of a page
    /// on its way while it was made: each time is one such step, of which
    /// there are as many as cells that dropped subtrees left.
    fn find(
        &mut self,
        key: Option<&[u8]>,
        back: bool,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        loop {
            if let Some(found) = self.land(key, back)?.settled() {
                return Ok(found);
            }
        }
    }

    /// Goes one node on, or with `back` one node back.
    fn step(&mut self, back: bool) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let Some(here) = self.cursor.here else {
            return Ok(None);
        };
        let moved = match self.resolve(None, here)? {
            Some(node) => self.advance(node, back)?,
            None => Moved::Lost,
        };
        if let Some(found) = moved.settled() {
            return Ok(found);
        }
        let key = self.cursor.take_key();
        let moved = self.pass(&key, back);
        self.cursor.sought = key;
        moved
    }

    /// The node the cursor stands on.
    fn read(&mut self) -> Result<Option<PinnedNode<'r>>, CursorError> {
        let Some(here) = self.cursor.here else {
            return Ok(None);
        };
        if let Some(node) = self.resolve(None, here)? {
            return Ok(Some(node));
        }
        let key = self.cursor.take_key();
        self.cursor.stats.refinds += 1;
        let found = self.find(Some(&key), false);
        let there = self.cursor.key == key;
        self.cursor.sought = key;
        Ok(found?.filter(|_| there))
    }

    /// Finds `key` again from the root and goes on from it: to the first
    /// node after `key`, or with `back` to the last one before it.
    fn pass(&mut self, key: &[u8], back: bool) -> Result<Option<PinnedNode<'r>>, CursorError> {
        self.cursor.stats.refinds += 1;
        loop {
            let moved = match self.land(Some(key), back)? {
                Moved::To(node) if self.cursor.key == key => self.advance(node, back)?,
                moved => moved,
            };
            if let Some(found) = moved.settled() {
                return Ok(found);
            }
        }
    }

    /// Walks down from the root to `key` and stands on the node that has
    /// it. If none has, on the nearest node on the way that has a greater
    /// key, or with `back` a smaller one.
    fn land(&mut self, key: Option<&[u8]>, back: bool) -> Result<Moved<'r>, CursorError> {
        let (_, from) = sides(back);
        self.cursor.leave();
        self.cursor.stats.descents += 1;
        let Some(root) = self.cursor.root else {
            return Ok(Moved::End);
        };
        let down = self.descend(None, root, key, from)?;
        match down.found {
            Some(found) => {
                let node = down.held.into_node(found);
                Ok(self.arrive(node.expect("the handle was read from this page")))
            }
            None => self.ascend(down.held, from, false),
        }
    }

    /// Goes from `node`, which the cursor stands on, to the node beside
    /// it: down to the nearest node below it on that side, or, where it
    /// has no child there, up to the nearest node that it lies on the
    /// other side of.
    fn advance(&mut self, node: PinnedNode<'r>, back: bool) -> Result<Moved<'r>, CursorError> {
        let (to, from) = sides(back);
        let here = node.handle();
        let below = node.node().link(to).map(|link| link.child);
        let mut held = node.into_page();
        if let Some(below) = below {
            self.remember(here, to)?;
            held = self.descend(Some(held), below, None, from)?.held;
        }
        self.ascend(held, from, true)
    }

    /// Walks down from the node at `from`: toward `key`, or with no key
    /// along the `edge` side, to a node that has no child that way. Every
    /// node it leaves is remembered, with the side it left on. It stops at
    /// a node that has `key`, which is not remembered.
    fn descend(
        &mut self,
        mut held: Option<PinnedPage<'r>>,
        mut from: CellAddress,
        key: Option<&[u8]>,
        edge: Side,
    ) -> Result<Down<'r>, CursorError> {
        loop {
            let page = self.page(held.take(), from.page())?;
            let node = page
                .view()
                .node(from.slot())
                .ok_or(NodeError::NoNode { address: from })?;
            let handle = node.handle();
            let side = match key.map(|key| key.cmp(node.key())) {
                Some(Ordering::Equal) => {
                    return Ok(Down {
                        held: page,
                        found: Some(handle),
                    });
                }
                Some(Ordering::Less) => Side::Left,
                Some(Ordering::Greater) => Side::Right,
                None => edge,
            };
            let below = node.link(side).map(|link| link.child);
            self.remember(handle, side)?;
            let Some(below) = below else {
                return Ok(Down {
                    held: page,
                    found: None,
                });
            };
            (held, from) = (Some(page), below);
        }
    }

    /// Goes up to the nearest remembered node that the way left on the
    /// `side` side, and stands on it. With `stepping`, the node has to lie
    /// on that side of the key the cursor has.
    fn ascend(
        &mut self,
        held: PinnedPage<'r>,
        side: Side,
        stepping: bool,
    ) -> Result<Moved<'r>, CursorError> {
        let above = loop {
            match self.cursor.way.pop() {
                Some(above) if above.side == side => break above.node,
                Some(_) => {}
                None => return Ok(Moved::End),
            }
        };
        let Some(node) = self.resolve(Some(held), above)? else {
            return Ok(Moved::Lost);
        };
        // A step that left a node on its `side` side came from smaller
        // keys if that side is the left one, and from greater ones if not.
        let expected = match side {
            Side::Left => Ordering::Greater,
            Side::Right => Ordering::Less,
        };
        if stepping && node.node().key().cmp(&self.cursor.key) != expected {
            return Err(CursorError::OutOfOrder {
                at: node.node().address(),
                back: side == Side::Right,
            });
        }
        Ok(self.arrive(node))
    }

    /// Stands on `node`.
    fn arrive(&mut self, node: PinnedNode<'r>) -> Moved<'r> {
        let view = node.node();
        self.cursor.here = Some(view.handle());
        self.cursor.key.clear();
        self.cursor.key.extend_from_slice(view.key());
        Moved::To(node)
    }

    /// Remembers that the way leaves `node` on `side`.
    fn remember(&mut self, node: NodeHandle, side: Side) -> Result<(), CursorError> {
        if self.cursor.way.len() >= Cursor::MAX_DEPTH {
            return Err(CursorError::TooDeep {
                at: node.address(),
                limit: Cursor::MAX_DEPTH,
            });
        }
        self.cursor.way.push(Above { node, side });
        Ok(())
    }

    /// The page `page`, pinned: `held` if that is the page, and otherwise
    /// asked of the reader once `held` is let go.
    fn page(
        &mut self,
        held: Option<PinnedPage<'r>>,
        page: PageId,
    ) -> Result<PinnedPage<'r>, CursorError> {
        match held {
            Some(held) if held.id() == page => Ok(held),
            held => {
                drop(held);
                self.cursor.stats.lookups += 1;
                Ok(self.reader.pin(page, self.buffers)?)
            }
        }
    }

    /// The node `handle` names, pinned, or `None` if the handle has ended:
    /// in `held` if that is its page, and otherwise asked of the reader
    /// once `held` is let go.
    fn resolve(
        &mut self,
        held: Option<PinnedPage<'r>>,
        handle: NodeHandle,
    ) -> Result<Option<PinnedNode<'r>>, CursorError> {
        match held {
            Some(held) if held.id() == handle.page() => Ok(held.into_node(handle)),
            held => {
                drop(held);
                self.cursor.stats.lookups += 1;
                Ok(self.reader.resolve(handle, self.buffers)?)
            }
        }
    }
}
