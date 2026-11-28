//! The parameters of the layout (`tree-layout.md`, section 12).

use lignin_format::PageSize;

use super::error::LayoutError;

/// Which fragments a write measures against the largest size (L1 and L4 of
/// `tree-layout.md`; its section 14.6 has the measurements).
///
/// # What it decides
///
/// A fragment is the cells of one page that hang together: a cell, and
/// below it whatever is reached without leaving the page. L1 keeps a
/// fragment within the largest size: where a write finds one above it, the
/// root of the fragment is carried to the page of its parent. To find one,
/// the write counts the cells of a fragment, up to that size.
///
/// A write names every node whose record changes, and for most of them
/// what changes is a hash. A value that is put into a Merk gives a new
/// hash to every node from its own up to the root of the grove. Such a
/// node's cell is **rewritten in place**: every part of it is as long as
/// it was, and every link and hosted-root pointer names the cell it named.
/// A fragment in which a write does that and nothing else is as large
/// after the write as it was before.
///
/// - [`Upkeep::Written`] measures the fragment of every node of the write.
/// - [`Upkeep::Changed`] measures the fragments in which the write changed
///   a cell, and passes over a cell that is rewritten in place.
///
/// # What it does not decide
///
/// **What a node holds.** Under either value a write puts the same cells
/// into the pages. Which page a cell is in can differ, as it can between
/// any two settings of the layout.
///
/// **The format.** No page records the value (F5). Pages written under one
/// are read and written under the other.
///
/// **Any other rule.** A page above its capacity is settled (section 6.2),
/// a small fragment goes to its parent's page when that has room (M1), and
/// a page that a delete left nearly empty gives its fragments away (M2),
/// for every fragment that holds a node of the write, under either value.
/// Whether one of those applies is not up to the cells of the fragment
/// alone. A parent's page has room once another write took cells out of
/// it, so a write that changes nothing of a small fragment can be the
/// first to find the room. The largest size is up to the cells of the
/// fragment alone, which is why it is the one rule that can be asked of
/// fewer fragments.
///
/// # What `Changed` gives up
///
/// Under `Written` every node of a write has a turn at L1, and each turn
/// sends up one root at most. Under `Changed` the turns of the cells that
/// are rewritten in place are passed over, and nothing is done in their
/// place. Two kinds of fragment stay above the largest size for longer:
///
/// - What is left below a root that went up. A cell grows, the root of its
///   fragment goes up, and what is left is still above the size: under
///   `Written` a node of the write below that root, which the write
///   rewrites in place, has its turn and sends the next root up.
/// - A fragment that a rule joined to another after its turn (section
///   6.2, M1 and M2 run after L1, and L1 brings roots into pages itself).
///   Under `Written` the next write that names a node of it has a turn
///   for it.
///
/// Under `Changed` either stays as it is until a write has a turn for
/// it: a write that changes a cell of it, or one that is planned once
/// more (below). Neither value brings a fragment within the size in one
/// write for certain: a turn sends up one root, and what is left below it
/// has a turn only where it holds a node of the write that has one. Such a
/// fragment is never larger than its page, which is settled under either
/// value. The same holds for a store whose largest size is set lower than
/// it was written with: `Written` gives a turn to the fragment of each
/// node that a write names, `Changed` to the fragment of each cell that a
/// write changes.
///
/// # Where a write finds no room
///
/// A write that grows a node which nothing in it points at can be refused
/// for room ([`FragmentError::NoRoom`](super::FragmentError::NoRoom), T3
/// of `tree-layout.md`): its page is full of cells the write does not
/// hold. Whether there is room is up to where the cells are, so it is up
/// to the layout, and the two values can differ in it. A turn of L1 can
/// bring a cell of the write into the full page, above cells that can
/// then leave with it, and a turn can as well take the last such cell out
/// of the page.
///
/// So where the first plan of a write under `Changed` finds no room, the
/// write is planned once more under `Written`, with every other parameter
/// of the layout, the pages, the hints and the hosts that are behind as
/// they were, and the result of that plan is the answer. `Changed`
/// therefore answers that there is no room only where `Written` does on
/// the same inputs. Some writes that `Written` refuses for room are taken
/// under `Changed`. The second plan has the turns of `Written`: a cell
/// that is rewritten in place is not passed over in it, so what `Changed`
/// passes over, it passes over in the first plan of a write.
///
/// No more than that is held, in two ways. Any other error can be met
/// under one value and not under the other: the two plans can read other
/// pages and ask for other new pages, so a page that cannot be read or
/// started can stop a write under one alone, and a write that names what
/// is not there can be found out under one alone. And it is said of one
/// write on the same inputs: two stores that took the same writes under
/// the two values can hold their cells in other pages, as under any two
/// settings of the layout, and a later write of that kind can then find
/// room in one and not in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Upkeep {
    /// The fragment of every node of the write, whatever the write does to
    /// the node. A write that rewrites a hash counts the fragment of each
    /// cell it rewrites, up to the largest size.
    Written,
    /// The fragments that hold a cell the write changed: a cell it
    /// created or carried, and a cell of which a part has another length
    /// or a pointer names another cell than before. A cell that is
    /// rewritten in place brings no fragment to the largest size. Its page
    /// is not read for how full it is either: the cell takes the bytes it
    /// took.
    Changed,
}

impl Upkeep {
    /// The value `tree-layout.md` proposes, and the one line that says
    /// which it is: [`LayoutParams::PROPOSED`] and [`Upkeep::default`] are
    /// this.
    pub const PROPOSED: Self = Self::Changed;
}

impl Default for Upkeep {
    fn default() -> Self {
        Self::PROPOSED
    }
}

/// The parameters that steer where cells go. None of them is part of the
/// format: pages written under one setting are read and written under any
/// other (`tree-layout.md` F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutParams {
    /// A fragment above this many percent of what a page holds is split
    /// where it is: its root goes up (L1). With 100 a fragment may fill its
    /// page.
    pub largest_fragment_percent: u32,
    /// A fragment smaller than the page size divided by this belongs in its
    /// parent's page (L2, V1, M1).
    pub small_fragment_divisor: u32,
    /// A page takes a fragment from another page only if that leaves it at
    /// most this many percent full (V4, M2). With 0 such a fragment always
    /// starts a fresh page.
    pub seed_limit_percent: u32,
    /// A page that a delete leaves less than this many percent full gives
    /// its fragments away (M2). With 0 none does.
    pub merge_below_percent: u32,
    /// Which fragments a write measures against the largest size (L4).
    pub upkeep: Upkeep,
}

impl LayoutParams {
    /// The values `tree-layout.md` proposes: a largest fragment of half a
    /// page, a small one of a sixteenth, a seed limit of 75% and a merge
    /// threshold of 35%, with the largest size measured in the fragments
    /// that a write changed ([`Upkeep::PROPOSED`]).
    pub const PROPOSED: Self = Self {
        largest_fragment_percent: 50,
        small_fragment_divisor: 16,
        seed_limit_percent: 75,
        merge_below_percent: 35,
        upkeep: Upkeep::PROPOSED,
    };
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self::PROPOSED
    }
}

/// The parameters of the layout for pages of one size, in bytes.
///
/// A size counts a cell with its slot, as a page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    size: PageSize,
    params: LayoutParams,
    capacity: usize,
    small: usize,
    largest: usize,
    seed_limit: usize,
    merge_below: usize,
}

impl Layout {
    /// The proposed layout for pages of `size`.
    pub fn proposed(size: PageSize) -> Self {
        Self::new(size, LayoutParams::PROPOSED).expect("the proposed parameters keep the rules")
    }

    /// The proposed layout for pages of `size`, with `upkeep` for which
    /// fragments a write measures against the largest size.
    pub fn proposed_under(size: PageSize, upkeep: Upkeep) -> Self {
        let params = LayoutParams {
            upkeep,
            ..LayoutParams::PROPOSED
        };
        Self::new(size, params).expect("the proposed parameters keep the rules")
    }

    /// The layout with `params` for pages of `size`, if the parameters keep
    /// the rules between them (`tree-layout.md`, section 12).
    pub fn new(size: PageSize, params: LayoutParams) -> Result<Self, LayoutError> {
        if params.small_fragment_divisor < 2 {
            return Err(LayoutError::SmallFragment);
        }
        if !(10..=100).contains(&params.largest_fragment_percent) {
            return Err(LayoutError::LargestFragment);
        }
        if params.seed_limit_percent > 100 {
            return Err(LayoutError::SeedLimit);
        }
        if params.merge_below_percent > params.seed_limit_percent {
            return Err(LayoutError::MergeThreshold);
        }
        let capacity = size.cell_capacity();
        let percent = |percent: u32| capacity * percent as usize / 100;
        let layout = Self {
            size,
            params,
            capacity,
            small: size.get() / params.small_fragment_divisor as usize,
            largest: percent(params.largest_fragment_percent),
            seed_limit: percent(params.seed_limit_percent),
            merge_below: percent(params.merge_below_percent),
        };
        if layout.small * 3 > layout.largest {
            return Err(LayoutError::SmallAgainstLargest);
        }
        Ok(layout)
    }

    /// The size of the pages.
    pub const fn page_size(&self) -> PageSize {
        self.size
    }

    /// The parameters the layout was made from.
    pub const fn params(&self) -> LayoutParams {
        self.params
    }

    /// The bytes of a page that cells and their slots can use.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The size below which a fragment belongs in its parent's page.
    pub const fn small_fragment(&self) -> usize {
        self.small
    }

    /// The size above which a fragment is split where it is.
    pub const fn largest_fragment(&self) -> usize {
        self.largest
    }

    /// The most bytes a page may hold after it took a fragment from another
    /// page.
    pub const fn seed_limit(&self) -> usize {
        self.seed_limit
    }

    /// The bytes below which a page that a delete shrank gives its fragments
    /// away.
    pub const fn merge_below(&self) -> usize {
        self.merge_below
    }

    /// Which fragments a write measures against the largest size.
    pub const fn upkeep(&self) -> Upkeep {
        self.params.upkeep
    }

    /// This layout with `upkeep` in the place of its own, and every other
    /// parameter as it is.
    #[must_use]
    pub const fn under(mut self, upkeep: Upkeep) -> Self {
        self.params.upkeep = upkeep;
        self
    }
}
