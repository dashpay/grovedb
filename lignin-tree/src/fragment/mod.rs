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
//! cell holds afterwards ([`NodeWrite`]), and the cells of the nodes it
//! deletes. A link or a hosted-root pointer names its node as a [`Target`]:
//! by the address its cell had before the write, or by its position in the
//! write, which is the only name a new node has. The addresses that the
//! pages store afterwards are this module's to give.
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
//!    [`Pages`], leaves the pages between two states: the caller rolls the
//!    write's scope back.
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
//! name other nodes, and a link is written where it is.
//!
//! # A node that nothing in the write points at
//!
//! Such a node is the root of its fragment for the rules, whatever else its
//! page holds, and it is not carried: the write does not hold the cell that
//! points at it (T3). What hangs below it can leave. The one exception is a
//! node that a [`RootWrite`] names: its pointer is the caller's, so it is
//! carried like any other, and the report says where to.
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
