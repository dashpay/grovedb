//! Fragment maintenance: which page each node's cell is in, and how it gets
//! there.
//!
//! - [`Fragments`] applies a [`Write`], the change sets of one operation, to
//!   the tree pages: it puts the cells of new nodes into pages, takes out
//!   those of deleted ones, changes the cells of the others, and then moves
//!   cells between pages by the rules of `docs/lignindb/tree-layout.md`,
//!   sections 5 to 8.
//! - [`Pages`] is how it reaches the pages, and [`Values`] is what it tells
//!   about the values that are not in their cells.
//! - [`Report`] says where every node of the write is afterwards, and which
//!   other cells were carried.
//! - [`Layout`] holds the parameters of section 12, which are
//!   configuration, and [`Hints`] the used bytes of the pages that were
//!   seen.
//!
//! The shape of every tree is its writer's. Nothing here reads a key to
//! decide anything, or changes a key, a value, a hash or which node a link
//! names. What changes is the page and the slot of a cell, and with them
//! the addresses that name it and the mark that says whether it is the root
//! of a fragment.
//!
//! # A write
//!
//! A write states, for every node whose cell is new or may differ, what the
//! cell holds afterwards ([`NodeWrite`]), the cells of the nodes it deletes,
//! and the roots of the Merks it drops or clears. A link or a hosted-root
//! pointer names its node as a [`Target`]: by the address its cell had
//! before the write, or by its position in the write, which is the only
//! name a new node has. The addresses that the pages store afterwards are
//! this module's to give.
//!
//! **What a write lets go of, it says** ([`Write::dropped`]). A pointer
//! that names another node than before does not show whether the node it
//! named is still in a tree: a host that is written after a write stopped
//! below it names a new root, and the old one hangs below that
//! (`tree-layout.md` T3). So nothing is taken for dropped that the write
//! does not name as dropped. The cells of a dropped Merk stay in their
//! pages (D1), and its root is marked as the root of its fragment.
//!
//! A write is applied in two steps.
//!
//! 1. **The plan.** The cells the write names are read, and the rules are
//!    applied to what was read, in the order `tree-layout.md` gives them:
//!    removals and R2, placement from the top down (P1 to P5), L1 and L2,
//!    V1 to V6, M1, M2, and V1 to V6 again. A rule looks at a page as the
//!    whole write leaves it (P5). Nothing is changed in this step, and a
//!    write that is refused, with [`FragmentError::Write`] or
//!    [`FragmentError::NoRoom`], has changed no page.
//! 2. **The changes.** Each cell is taken from the page it is in to the
//!    page the plan ends with, at most once, and a cell that the rules
//!    moved away and back is not touched. Every change is a call of
//!    [`NodePageMut`](crate::node::NodePageMut), so it is told to the
//!    page's [`ChangeSink`](crate::node::ChangeSink) as the record that
//!    makes it. A carried cell is a remove record in one page and an insert
//!    record in the other. An error in this step, which can only come from
//!    [`Pages`], leaves the pages between two states. The caller rolls the
//!    write's scope back, and with it what the sinks of the pages and
//!    [`Values`] were told.
//!
//! # Allocation inside a page
//!
//! - **Slots.** A cell that arrives takes the lowest free slot of its page
//!   (S4). A slot is freed only by a removal, and a removal ends the
//!   handles into the page (`tree-page.md` TP7).
//! - **Room.** A page holds its cells and a slot directory as long as its
//!   highest slot in use. The plan counts both for every page it changes,
//!   so "above its capacity" is exactly "the page would refuse a cell".
//! - **Order.** All removals of a write, and every part of a staying cell
//!   that gets shorter, are made before anything is put in or grows. So a
//!   page never holds more than it holds at the end of the write, and no
//!   change is refused for want of room that the write itself makes.
//! - **Bytes.** Where a cell lies inside its page is the page codec's: a
//!   cell goes to the end of the page, and the page gathers its unused
//!   bytes when the end has no room (`tree-page.md` W2).
//!
//! # What a carry does
//!
//! A cell is carried by removing it from its page and inserting its content
//! into the other. With it change, in the same write:
//!
//! - **The address in the cell that points at it** (S3): the link of its
//!   parent or the pointer of its host. The write knows that cell, because
//!   it only carries what it reached from a cell it holds (F4). A root that
//!   no cell points at is named by a [`RootWrite`], and the report gives
//!   its address. The directory entry of a hosted Merk whose root is
//!   carried is the caller's to rewrite: [`Carried::hosted_by`] names it.
//! - **The fragment-root marks** (F3) of the cell, of the cells it points
//!   at and of the cells that pointed at it.
//! - **Its value, if that is out of line** (X1, `value-log-format.md` O5).
//!   The cell is inserted with a pending value, never with the pointer it
//!   had: [`Values::carried`] is told the place it held, the page it left
//!   and the page it is in, and returns what stands in the new cell. The
//!   commit repeats the value's bytes with the new page as their owner, and
//!   write-back puts the pointer into the cell (record 21 of
//!   `page-delta.md`).
//!
//! A rotation carries nothing (R1): it reaches this module as links that
//! name other nodes, and a link is written where it is. The rules of
//! sections 6 and 7 then apply to the fragments as the rotation left them,
//! in the same write, as they do in the simulation: a fragment that the
//! rotation left small, with its parent in another page that has room, is
//! carried there by M1 and counted under [`Cause::Merge`].
//!
//! # A node that nothing in the write points at
//!
//! Such a node is the root of its fragment for the rules, whatever else its
//! page holds, and it is not carried: the write does not hold the cell that
//! points at it (T3). What hangs below it can leave. The one exception is a
//! node that a [`RootWrite`] names without anchoring it: its pointer is
//! the caller's alone, so it is carried like any other, and the report
//! says where to.
//!
//! A write that stops below the host of its Merk names the Merk's root in
//! a [`RootWrite`] that is [anchored](RootWrite::anchored). The root then
//! stays, the report gives its address for the directory entry, and a root
//! that is another node than before is marked as the root of its fragment:
//! the pointer in the host's cell does not name it yet. The write that
//! later holds the host states the root as the host's, as any write does.
//!
//! Until then the pointers in the host's cell are behind, and they are not
//! followed ([`Pages::host_is_behind`], `transactions.md` SC9). This is
//! one rule, kept in one place: to the plan of a write, the pointers of a
//! host that is behind name nothing.
//!
//! - Nothing is read through them, nothing is taken for a part of the
//!   host's fragment and nothing is carried with the host.
//! - Nothing is taken to have been pointed at by them: no node takes the
//!   page of a deleted node that such a pointer named (R2). That holds
//!   where the pointer was exact as well, which the plan cannot know: a
//!   node that becomes the root of a Merk whose host is behind stays where
//!   it is, as it does in a write that stops below the host (T3).
//! - They keep their bytes: in a host that is carried, and in a host that
//!   is written and keeps them ([`HostedWrite::Keep`], [`Target::Kept`]).
//!
//! The fragment-root mark of the root can be behind with the pointer, in
//! the same bounds: the host may leave the root's page, in a write that
//! does not follow the pointer. The write that names the roots again, each
//! by its address or by its place in the write ([`HostedWrite::Set`]), puts
//! both right: in the host itself, or in the new cell that takes its Merks
//! over when the host moves to another key (D5). A write that meets a host
//! that is behind reads the mark of every root that one of its hosts names
//! so and that stays in another page than that host. It sets the mark where
//! it is not set, in a page it may change for nothing else. A root that is
//! dropped while its host is behind is marked in the same way, by the write
//! that drops it.
//!
//! **A drop comes after the hosts inside it.** A Merk that is dropped
//! while a host inside it is behind would keep that host's pointer as it
//! is for good: the cell is not written again, the reclaimer walks it
//! (D2), and its page would not pass the check of its fragment roots in a
//! committed state. So the caller writes every such host with its roots
//! before the write that drops the Merk. This is an exception to "a drop
//! writes the page of the host and one directory entry" (D1, D4,
//! `transactions.md` CV9): the transaction that drops the Merk also writes
//! one page for each host inside it that is behind. Nothing here can see
//! that it was left out.
//!
//! On existing GroveVersions every write holds the whole path up to the
//! root of the grove, and the root is named by a root pointer, so no node
//! is in that position. From iteration two a write can stop below the host
//! of its Merk. If the root of that Merk then grows in a page that is full
//! of cells the write does not hold, no rule can make room, and the write
//! is refused with [`FragmentError::NoRoom`].
//!
//! # What the callers do
//!
//! - **One writer.** A write is planned on the pages as they are and then
//!   applied, so nothing else changes those pages in between. A write scope
//!   has that by its private copies (`transactions.md` LT4).
//! - **Addresses are exact.** An address in a [`Write`] is one the pages
//!   store, or one a report of the same scope gave. A [`Target::At`] that
//!   names no cell is written into a link as it is.
//! - **The whole operation.** The nodes of all Merks that one operation
//!   wrote are one write, with each host pointing at the root of the Merk
//!   it hosts (P5).
//! - **Handles.** A removal ends every handle into its page. The report
//!   lists the pages a cell left and gives the handles of the write's nodes
//!   and of every cell that was carried.
//! - **Applying a scope's writes again** on pages that another scope
//!   changed (LT4) is applying the same [`Write`] with the addresses the
//!   merged pages have. The rules are a function of the pages, the write
//!   and the hints (T4), so the result is a valid layout, and need not be
//!   the one the scope had.
//!
//! # What is not here yet
//!
//! A small Merk that travels with its host is carried whoever has claimed
//! it. The exception of `tree-layout.md` T2, which stops at the claim of
//! another open scope, needs the claims of TXN-09.

mod apply;
mod error;
mod layout;
mod pages;
mod plan;
mod report;
mod write;

#[cfg(test)]
mod tests;

pub use error::{FragmentError, LayoutError, WriteError};
pub use layout::{Layout, LayoutParams};
pub use pages::{Hints, PageMut, Pages, Values};
pub use report::{Carried, Carries, Cause, Counters, Moves, Report};
pub use write::{HostedWrite, LinkWrite, NodeWrite, RootWrite, Target, ValueWrite, Write};

/// Keeps the cells of Merk nodes in their pages: applies writes, and moves
/// cells between pages as `docs/lignindb/tree-layout.md` says.
///
/// It holds no page and no tree. Between two writes it keeps the layout's
/// parameters, the hints and its counters.
#[derive(Debug, Clone)]
pub struct Fragments {
    layout: Layout,
    hints: Hints,
    counters: Counters,
}

impl Fragments {
    /// Fragment maintenance under `layout`, with no hint yet.
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            hints: Hints::new(),
            counters: Counters::default(),
        }
    }

    /// The parameters the next write is laid out with.
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Sets the parameters for the writes that follow. The pages need not
    /// have been written under them (`tree-layout.md` F5).
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    /// The used bytes of the pages that were seen.
    pub const fn hints(&self) -> &Hints {
        &self.hints
    }

    /// The hints, to add what a reader saw or to drop what a rollback made
    /// wrong.
    pub fn hints_mut(&mut self) -> &mut Hints {
        &mut self.hints
    }

    /// What the writes so far did.
    pub const fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Applies one write to the pages of `pages`, whose size is the
    /// layout's.
    ///
    /// On [`FragmentError::Write`] and [`FragmentError::NoRoom`] no page was
    /// changed. On any other error the pages may hold a part of the write.
    pub fn apply<P: Pages, V: Values>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        write: &Write<'_>,
    ) -> Result<Report, FragmentError> {
        let plan = plan::plan(&self.layout, &self.hints, pages, write)?;
        let report = apply::apply(&plan, pages, values, &mut self.hints)?;
        let created = write.nodes.iter().filter(|node| node.at.is_none()).count();
        self.counters.count(&report, created, write.removed.len());
        Ok(report)
    }
}
