//! The parameters of the layout (`tree-layout.md`, section 12).

use lignin_format::PageSize;

use super::error::LayoutError;

/// The four parameters that steer where cells go. None of them is part of
/// the format: pages written under one setting are read and written under
/// any other (`tree-layout.md` F5).
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
}

impl LayoutParams {
    /// The values `tree-layout.md` proposes: a largest fragment of half a
    /// page, a small one of a sixteenth, a seed limit of 75% and a merge
    /// threshold of 35%.
    pub const PROPOSED: Self = Self {
        largest_fragment_percent: 50,
        small_fragment_divisor: 16,
        seed_limit_percent: 75,
        merge_below_percent: 35,
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
}
