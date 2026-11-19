//! Which values stay in their cell.

use lignin_format::{tree_page::TreeCell, PageSize};

use super::error::ValueError;

/// The inline threshold of tree cells: a value of up to that many bytes is
/// stored in its cell, and a longer one goes out of line.
///
/// The decision reads the value's length and nothing else. So a value never
/// changes sides because its cell's key, feature or links changed, and a
/// writer knows where a value goes before it has a cell for it.
///
/// The threshold is configuration, not format (`tree-layout.md` F5): a cell
/// keeps the form its value was written in, whatever the threshold is
/// later. See the [module documentation](super) for why it is 256 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlinePolicy {
    threshold: usize,
}

impl InlinePolicy {
    /// The threshold of [`Self::DEFAULT`], in bytes.
    pub const DEFAULT_THRESHOLD: usize = 256;

    /// The smallest threshold: the place of a value out of line takes this
    /// many bytes of its cell, so a shorter value gains nothing by leaving.
    pub const MIN_THRESHOLD: usize = TreeCell::VALUE_LOCATION_LEN;

    /// The policy of a store that sets no other: valid for every page size.
    pub const DEFAULT: Self = Self {
        threshold: Self::DEFAULT_THRESHOLD,
    };

    /// The largest threshold a store of pages of `page_size` can have: a
    /// value of that length fits in its cell whatever else the cell holds,
    /// so the cell with its slot stays under half a page (`tree-page.md`
    /// TP14, `tree-layout.md` C7).
    pub const fn max_threshold(page_size: PageSize) -> usize {
        let others = TreeCell::MAX_OUT_OF_LINE_LEN - TreeCell::VALUE_LOCATION_LEN;
        page_size.max_inline_value_len(others)
    }

    /// A policy with the threshold `threshold`, for a store of pages of
    /// `page_size`: from [`Self::MIN_THRESHOLD`] to
    /// [`Self::max_threshold`].
    pub const fn new(page_size: PageSize, threshold: usize) -> Result<Self, ValueError> {
        let max = Self::max_threshold(page_size);
        if threshold < Self::MIN_THRESHOLD || threshold > max {
            return Err(ValueError::Threshold {
                threshold,
                min: Self::MIN_THRESHOLD,
                max,
            });
        }
        Ok(Self { threshold })
    }

    /// The longest value that is stored in its cell.
    pub const fn threshold(&self) -> usize {
        self.threshold
    }

    /// Whether a value of `value_len` bytes is stored in its cell. An empty
    /// value always is (`value-log-format.md` V2).
    pub const fn is_inline(&self, value_len: usize) -> bool {
        value_len <= self.threshold
    }
}

impl Default for InlinePolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}
