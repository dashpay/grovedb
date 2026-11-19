//! What fragment maintenance asks of the write it works for: the pages, and
//! the values that are out of line.

use std::collections::HashMap;

use lignin_format::{
    tree_page::TreePage,
    value_log::{PendingValue, ValuePlace},
    CellAddress, PageId,
};

use super::error::FragmentError;
use crate::node::{ChangeSink, NodeError, NodePage, NodePageMut};

/// A tree page that is open for the changes of one physical step, with
/// whoever is told of them.
pub type PageMut<'p, 'f> = NodePageMut<'f, &'p mut dyn ChangeSink>;

/// The pages as one write reads and changes them: the private copies of a
/// write scope, laid over the committed pages it reads.
///
/// Each call is one physical step on one page (`transactions.md` LT3):
/// fragment maintenance never holds two pages at once, so its caller never
/// holds two latches for it.
///
/// A call that fails returns [`FragmentError::Pages`] or
/// [`FragmentError::Node`], and no other error: the others say that no
/// page was changed, which a failure in the middle of a write cannot say.
pub trait Pages {
    /// Reads the tree page `page` as the write sees it.
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError>;

    /// Changes the tree page `page`: its private copy, which is made if the
    /// write has none yet. Every change `change` makes is told to the
    /// page's [`ChangeSink`].
    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError>;

    /// Starts an empty tree page under an id that names no other page of
    /// the write, and returns the id.
    fn create(&mut self) -> Result<PageId, FragmentError>;

    /// The page `page` holds no cell any more (`tree-layout.md` M3). Its id
    /// names no other page before the write has committed (S4).
    fn free(&mut self, page: PageId) -> Result<(), FragmentError>;

    /// Whether the hosted-root pointers of the cell at `host` may be behind
    /// the roots of the Merks it hosts: its element is marked pending
    /// (`transactions.md` SC9), because a write stopped below it and none
    /// has written it since (`tree-layout.md` T3).
    ///
    /// Such a pointer is not followed: to the plan of a write it names
    /// nothing. What it names is not taken for a part of the host's
    /// fragment and is not carried with it, and the pointer keeps its bytes
    /// until a write names the host's roots again
    /// ([`HostedWrite::Set`](super::HostedWrite::Set)).
    ///
    /// The answer is `true` from the write that stopped below the host up
    /// to and through the write that names the host's roots again, and
    /// `false` before any write that follows that one: finalize rewrites
    /// the pointer and then removes the mark (SC9). The write that names
    /// the roots is the one that puts their marks right.
    ///
    /// No element is marked on existing GroveVersions, so there the answer
    /// is `false`, which is what this gives unless it is replaced.
    fn host_is_behind(&mut self, host: CellAddress) -> bool {
        let _ = host;
        false
    }
}

/// The values of the cells a write touches that are not in their cells.
///
/// A value log record names the page that owns the value and is written
/// once (`value-log-format.md` O1, O5). So every call here says which page
/// a value belongs to now, or that a cell stopped holding one, and the
/// write's commit acts on all of them together: it repeats the bytes of each
/// carried value in its log group with the new page as the owner, and
/// releases each pointer a cell stopped holding.
pub trait Values {
    /// A cell of `page` held `place` and holds it no more: its node was
    /// deleted, or the write gave it another value.
    fn released(&mut self, place: ValuePlace, page: PageId);

    /// A cell of `page` now holds the pending value `pending`, which the
    /// write gave it ([`ValueWrite::Pending`](super::ValueWrite::Pending)):
    /// `page` is the value's owner.
    fn placed(&mut self, pending: PendingValue, page: PageId);

    /// A cell that held `place` in `from` was carried to `to`, with its
    /// value. Returns what stands in the cell in `to`: a pending value of
    /// the same length, whose bytes the commit appends again with `to` as
    /// their owner. `from` holds `place` no more, and nothing else reports
    /// that.
    fn carried(&mut self, place: ValuePlace, from: PageId, to: PageId) -> PendingValue;
}

/// How many bytes the pages that were seen hold, by page: a hint
/// (`tree-layout.md`, section 17 item 14).
///
/// A fragment that leaves its page goes to the fullest page beside it that
/// has room (V4, M2). The pages beside it are mostly ones the write has not
/// read, and it reads none to make the choice: a page with no hint is not
/// chosen. A hint may be out of date, after a rollback or after cells were
/// taken out by background work. The page that is chosen is read before
/// anything goes to it, so a wrong hint costs fill, or one read of a page
/// that then turns out to be too full, and nothing else.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    used: HashMap<PageId, u32>,
}

impl Hints {
    /// No hint.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes the cells of `page` and their slots took when it was last
    /// seen.
    pub fn used(&self, page: PageId) -> Option<usize> {
        self.used.get(&page).map(|used| *used as usize)
    }

    /// Takes the hint of a page that is being read anyway.
    pub fn see(&mut self, page: &NodePage<'_>) {
        self.note(page.id(), used_bytes(page));
    }

    /// Sets the hint of `page` to `used` bytes of cells and slots.
    pub fn note(&mut self, page: PageId, used: usize) {
        self.used.insert(page, used as u32);
    }

    /// Drops the hint of `page`: the page was freed.
    pub fn forget(&mut self, page: PageId) {
        self.used.remove(&page);
    }

    /// Drops every hint.
    pub fn clear(&mut self) {
        self.used.clear();
    }

    /// How many pages have a hint.
    pub fn len(&self) -> usize {
        self.used.len()
    }

    /// Whether no page has a hint.
    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }
}

/// The bytes the cells of a page and their slots take.
pub(super) fn used_bytes(page: &NodePage<'_>) -> usize {
    page.tree_page().used_bytes() - TreePage::HEADER_LEN
}
