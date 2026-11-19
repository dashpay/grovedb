//! The order in which a scan reaches the cells of one page, and how far the
//! page's values in the value log are from that order.

use std::collections::HashSet;

use lignin_format::{
    tree_page::{Side, TreeCell, TreePage},
    value_log::ValuePointer,
    CellAddress, FormatVersion, PageId, SlotId,
};

enum Visit {
    Enter(SlotId),
    Emit(SlotId),
}

fn in_page(page: PageId, address: Option<CellAddress>) -> Option<SlotId> {
    address
        .filter(|address| address.page() == page)
        .map(|address| address.slot())
}

fn child(page: PageId, cell: &TreeCell<'_>, side: Side) -> Option<SlotId> {
    in_page(page, cell.link(side).map(|link| link.child))
}

/// The cells of the fragment below `root` that are in the page, in key
/// order: the left child's, the cell, those of the Merks it hosts, the
/// right child's. A cell that was reached before is not reached again.
fn walk(page: &TreePage<'_>, root: SlotId, seen: &mut HashSet<SlotId>, out: &mut Vec<SlotId>) {
    let id = page.id();
    let mut stack = vec![Visit::Enter(root)];
    while let Some(visit) = stack.pop() {
        let slot = match visit {
            Visit::Emit(slot) => {
                out.push(slot);
                continue;
            }
            Visit::Enter(slot) => slot,
        };
        let Some(cell) = page.cell(slot).filter(|_| seen.insert(slot)) else {
            continue;
        };
        stack.extend(child(id, &cell, Side::Right).map(Visit::Enter));
        let hosted = cell.hosted_roots().rev();
        stack.extend(hosted.filter_map(|root| in_page(id, root).map(Visit::Enter)));
        stack.push(Visit::Emit(slot));
        stack.extend(child(id, &cell, Side::Left).map(Visit::Enter));
    }
}

/// The cells of `page` in the order a scan reaches them.
///
/// A scan walks one Merk in key order. Inside a page that is the order of
/// each fragment (`tree-layout.md`, section 4): a node after its left
/// subtree and before its right one, as far as they are in the page, and
/// the Merks a node hosts in its page directly behind it. The fragments
/// of the page follow each other in the order of their first keys.
///
/// The keys of two fragments are not compared beyond that: they can be of
/// different Merks, and where they are of one Merk a scan leaves the page
/// between them anyway.
pub fn scan_order(page: &TreePage<'_>) -> Vec<SlotId> {
    let mut seen = HashSet::with_capacity(page.cell_count());
    let mut fragments = Vec::new();
    let roots = page.fragment_roots();
    for start in roots.chain(page.cells().map(|(slot, _)| slot)) {
        let mut cells = Vec::new();
        walk(page, start, &mut seen, &mut cells);
        if let Some(&first) = cells.first() {
            fragments.push((first, cells));
        }
    }
    let key = |slot: SlotId| page.cell(slot).map(|cell| cell.key());
    fragments.sort_by(|(a, _), (b, _)| key(*a).cmp(&key(*b)).then(a.cmp(b)));
    fragments.into_iter().flat_map(|(_, cells)| cells).collect()
}

/// The value pointers the cells of `page` hold, in [`scan_order`].
pub(super) fn pointers_in_order(page: &TreePage<'_>) -> Vec<ValuePointer> {
    scan_order(page)
        .into_iter()
        .filter_map(|slot| page.cell(slot)?.value_pointer())
        .collect()
}

/// How the values of one page lie in the value log, against the order a
/// scan reads them in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Locality {
    /// How many cells of the page hold a value pointer.
    pub values: u64,
    /// How many runs those values are in. A run is a row of values that
    /// follow each other in the scan and lie back to back in one file, so
    /// that reading them in that order is one sequential read.
    pub runs: u64,
    /// The bytes those values take in their files, record headers included.
    pub bytes: u64,
}

impl Locality {
    /// The locality of the values of `page`.
    ///
    /// A value's extent is taken as the current format version gives it
    /// (`value-log-format.md` P5). In a file of another version a value
    /// can so be counted as the start of a run that it is not, which
    /// costs a regrouping and nothing else.
    pub fn of(page: &TreePage<'_>) -> Self {
        Self::of_pointers(&pointers_in_order(page))
    }

    pub(super) fn of_pointers(pointers: &[ValuePointer]) -> Self {
        let version = FormatVersion::CURRENT;
        let mut locality = Self::default();
        let mut end = None;
        for pointer in pointers {
            locality.values += 1;
            locality.bytes += pointer.extent_len(version);
            if end != Some((pointer.file(), pointer.offset())) {
                locality.runs += 1;
            }
            end = Some((pointer.file(), pointer.end(version)));
        }
        locality
    }

    /// How many times a scan of the page's values leaves the place it
    /// reads at for another one.
    pub const fn jumps(&self) -> u64 {
        self.runs.saturating_sub(1)
    }

    /// Whether a scan reads the page's values as one sequential read.
    pub const fn is_sequential(&self) -> bool {
        self.runs <= 1
    }
}

/// When the values of a page are regrouped.
///
/// Regrouping appends every value of the page again, so it is worth its
/// bytes only for a page whose values are scattered. The measure is the
/// share of the page's values that a scan has to jump to
/// ([`Locality::jumps`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegroupPolicy {
    min_jump_percent: u8,
}

impl RegroupPolicy {
    /// Regroups a page as soon as a scan of its values jumps once.
    pub const ALWAYS: Self = Self {
        min_jump_percent: 0,
    };

    /// Regroups a page when a scan jumps to at least a quarter of its
    /// values.
    pub const DEFAULT: Self = Self {
        min_jump_percent: 25,
    };

    /// Regroups a page when a scan jumps to at least `min_jump_percent` in
    /// a hundred of its values. From 100 on no page is regrouped: a scan
    /// jumps to fewer values than the page has.
    pub const fn new(min_jump_percent: u8) -> Self {
        Self { min_jump_percent }
    }

    /// Whether a page whose values lie as `locality` says is regrouped.
    pub const fn applies(&self, locality: Locality) -> bool {
        let jumps = locality.jumps();
        jumps != 0 && jumps * 100 >= locality.values * self.min_jump_percent as u64
    }
}

impl Default for RegroupPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}
