//! Tree pages: slotted pages of Merk node cells.
//!
//! See `docs/lignindb/tree-page.md`.
//!
//! ```text
//! offset  size  field of the page header
//!      0     1  page kind: 3
//!      1     1  reserved, 0
//!      2     2  length of the page in bytes
//!      4     4  page id
//!      8     2  slot count
//!     10     2  cell count
//!     12     2  bytes among the cells that belong to no cell: 0 in an image
//!     14     8  departures: how many cells have left the page
//!   then        the slot directory, 2 bytes a slot: the offset of the
//!               slot's cell, or 0 for a free slot
//!   then        the cells
//! ```
//!
//! ```text
//! offset  size  field of a cell
//!      0     1  flags: bit 0 left link, bit 1 right link, bit 2 value out
//!               of line, bit 3 fragment root; the rest 0
//!      1     1  key length
//!      2     1  feature length
//!      3     1  number of hosted-root pointers
//!      4     4  value length
//!      8    32  value hash
//!     40    32  key-value hash
//!   then        the left link, the right link (each only if its flag is set)
//!   then        the hosted-root pointers, 6 bytes each: a cell address or 0
//!   then        the feature, the key
//!   then        the value, or for a value out of line 12 bytes: the file id
//!               and the offset of its value pointer
//! ```
//!
//! ```text
//! offset  size  field of a link
//!      0     6  the child's cell address: page id, slot
//!      6    32  the child's hash
//!     38     2  the heights of the child's left and right subtree
//!     40     1  the length of the child's key
//!     41     1  aggregate length
//!     42        the child's aggregate
//! ```
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     tree_page::{CellContent, CellValue, HashField, TreePage, TreePageMut, TreePageSize},
//!     FormatVersion, PageId, SlotId,
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let size = TreePageSize::DEFAULT;
//! let mut frame = vec![0; size.get()];
//! let mut page = TreePageMut::init(&mut frame, size, PageId::MIN, 0)?;
//! page.insert(
//!     SlotId::MIN,
//!     &CellContent {
//!         key: b"key",
//!         feature: &[],
//!         value_hash: &[1; 32],
//!         kv_hash: &[2; 32],
//!         left: None,
//!         right: None,
//!         hosted_roots: &[],
//!         value: CellValue::Inline(b"value"),
//!         fragment_root: true,
//!     },
//! )?;
//!
//! // A hash is overwritten where it is; the value is not rewritten.
//! page.set_hash(SlotId::MIN, HashField::Value, &[9; 32])?;
//!
//! // The image is what a segment stores, and it reads back.
//! let image = page.page().to_image();
//! let page = TreePage::decode(&image, FormatVersion::CURRENT, size)?;
//! let cell = page.cell(SlotId::MIN).expect("the cell");
//! assert_eq!(cell.key(), b"key");
//! assert_eq!(cell.value_hash(), &[9; 32]);
//! assert_eq!(cell.value(), CellValue::Inline(b"value"));
//! # Ok(())
//! # }
//! ```

use std::fmt;

use crate::{error::FormatError, ids::SlotId};

mod cell;
mod page;
mod write;

#[cfg(test)]
mod tests;

pub use cell::{
    CellContent, CellField, CellValue, Hash, HashField, HostedRoots, Side, TreeCell, TreeLink,
    HASH_LEN,
};
pub use page::{Cells, TreePage};
pub use write::TreePageMut;

/// Why a byte string is not a tree page or a tree page cell, or why a tree
/// page cannot take what was written to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreePageError {
    /// A page size outside [`TreePageSize::MIN`] to [`TreePageSize::MAX`].
    PageSizeOutOfRange {
        /// The page size asked for.
        page_size: usize,
    },
    /// The buffer given for a page is smaller than a page of that size
    /// needs.
    BufferTooSmall {
        /// The buffer's length.
        len: usize,
        /// The bytes a page of that size needs.
        needed: usize,
    },
    /// The page length field is smaller than the page header.
    PageTooShort {
        /// The page length.
        len: usize,
    },
    /// The page is longer than a page of the given size may be.
    PageTooLong {
        /// The page length.
        len: usize,
        /// The longest page of that size.
        capacity: usize,
    },
    /// The slot directory does not fit in the page.
    DirectoryOutOfBounds {
        /// The offset at which the cells would start.
        needed: usize,
        /// The page length.
        len: usize,
    },
    /// The last slot of the directory is free.
    TrailingFreeSlot,
    /// A slot's offset does not lead to a cell that lies among the page's
    /// cells.
    InvalidSlot {
        /// The slot.
        slot: SlotId,
    },
    /// In an image, a cell does not start where the cell of the slot before
    /// it ends.
    CellsOutOfOrder {
        /// The slot whose cell is out of place.
        slot: SlotId,
    },
    /// In an image, the last cell ends before the image does.
    UnusedBytes {
        /// The offset at which the cells end.
        used: usize,
        /// The image length.
        len: usize,
    },
    /// The page states unused bytes among its cells, which an image never
    /// has.
    NotCompact {
        /// The unused bytes the page states.
        unused: usize,
    },
    /// The unused bytes the page states are not the bytes its cells leave
    /// unused.
    UnusedBytesMismatch {
        /// The unused bytes the page states.
        stated: usize,
        /// The bytes its cells leave unused.
        found: usize,
    },
    /// The cell count the page states is not its number of occupied slots.
    CellCountMismatch {
        /// The cell count the page states.
        stated: usize,
        /// The number of occupied slots.
        found: usize,
    },
    /// Two cells of the page share bytes.
    OverlappingCells {
        /// The slot of the cell that starts inside another.
        slot: SlotId,
    },
    /// A cell is as large as half of what a page can hold, or larger.
    CellTooLarge {
        /// The cell's length in bytes, without its slot.
        len: usize,
        /// The longest cell a page of that size takes.
        max: usize,
    },
    /// A part of a cell is longer than its length field can say.
    FieldTooLong {
        /// The part.
        field: CellField,
        /// Its length.
        len: usize,
        /// The longest it may be.
        max: usize,
    },
    /// The bytes of a cell go on after the cell ends.
    CellLengthMismatch {
        /// The cell's length by its own fields.
        expected: usize,
        /// The bytes given.
        found: usize,
    },
    /// The page has no room for what was written. The page is unchanged.
    PageFull {
        /// The free bytes the write needs.
        needed: usize,
        /// The free bytes the page has.
        available: usize,
    },
    /// A cell was put into a slot that holds one.
    SlotOccupied {
        /// The slot.
        slot: SlotId,
    },
    /// A slot that holds no cell was read as one.
    SlotFree {
        /// The slot.
        slot: SlotId,
    },
    /// A cell has no link on the side that was written.
    NoSuchLink {
        /// The cell's slot.
        slot: SlotId,
        /// The side.
        side: Side,
    },
    /// A cell has no hosted-root pointer at the position that was written.
    NoSuchHostedRoot {
        /// The cell's slot.
        slot: SlotId,
        /// The position.
        index: usize,
    },
    /// A link or a hosted-root pointer names a slot of its own page that
    /// holds no cell, or the cell it is in.
    DanglingAddress {
        /// The slot of the cell that holds the address.
        slot: SlotId,
    },
    /// Two links or hosted-root pointers of the page name the same cell of
    /// the page.
    CellNamedTwice {
        /// The slot of the cell that is named twice.
        slot: SlotId,
    },
    /// A cell is marked as a fragment root while a cell of the page points
    /// at it, or is not marked while none does.
    FragmentRootMismatch {
        /// The cell's slot.
        slot: SlotId,
    },
}

impl fmt::Display for TreePageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageSizeOutOfRange { page_size } => write!(
                f,
                "page size {page_size} is outside {} to {}",
                TreePageSize::MIN.get(),
                TreePageSize::MAX.get()
            ),
            Self::BufferTooSmall { len, needed } => {
                write!(f, "buffer of {len} bytes for a page of {needed}")
            }
            Self::PageTooShort { len } => write!(
                f,
                "page length {len} is below the {}-byte header",
                TreePage::HEADER_LEN
            ),
            Self::PageTooLong { len, capacity } => {
                write!(f, "page length {len} exceeds the capacity of {capacity}")
            }
            Self::DirectoryOutOfBounds { needed, len } => write!(
                f,
                "slot directory ends at {needed}, past the page length {len}"
            ),
            Self::TrailingFreeSlot => write!(f, "the last slot of the directory is free"),
            Self::InvalidSlot { slot } => write!(f, "slot {slot} does not lead to a cell"),
            Self::CellsOutOfOrder { slot } => write!(
                f,
                "the cell of slot {slot} does not start where the cell before it ends"
            ),
            Self::UnusedBytes { used, len } => {
                write!(f, "cells end at {used}, before the image length {len}")
            }
            Self::NotCompact { unused } => {
                write!(f, "{unused} unused bytes among the cells of an image")
            }
            Self::UnusedBytesMismatch { stated, found } => write!(
                f,
                "page states {stated} unused bytes among its cells, and has {found}"
            ),
            Self::CellCountMismatch { stated, found } => {
                write!(f, "page states {stated} cells, and has {found}")
            }
            Self::OverlappingCells { slot } => {
                write!(f, "the cell of slot {slot} starts inside another cell")
            }
            Self::CellTooLarge { len, max } => {
                write!(f, "cell of {len} bytes exceeds the maximum of {max}")
            }
            Self::FieldTooLong { field, len, max } => {
                write!(f, "{field} of {len} bytes exceeds the maximum of {max}")
            }
            Self::CellLengthMismatch { expected, found } => {
                write!(f, "cell of {expected} bytes given as {found} bytes")
            }
            Self::PageFull { needed, available } => {
                write!(f, "page full: need {needed} bytes, {available} are free")
            }
            Self::SlotOccupied { slot } => write!(f, "slot {slot} already holds a cell"),
            Self::SlotFree { slot } => write!(f, "slot {slot} holds no cell"),
            Self::NoSuchLink { slot, side } => {
                write!(f, "the cell of slot {slot} has no {side} link")
            }
            Self::NoSuchHostedRoot { slot, index } => write!(
                f,
                "the cell of slot {slot} has no hosted-root pointer {index}"
            ),
            Self::DanglingAddress { slot } => write!(
                f,
                "the cell of slot {slot} points at no cell of its own page"
            ),
            Self::CellNamedTwice { slot } => {
                write!(f, "two cells of the page point at the cell of slot {slot}")
            }
            Self::FragmentRootMismatch { slot } => write!(
                f,
                "the fragment-root mark of slot {slot} disagrees with the page's links"
            ),
        }
    }
}

impl std::error::Error for TreePageError {}

impl From<TreePageError> for FormatError {
    fn from(error: TreePageError) -> Self {
        Self::TreePage(error)
    }
}

/// The size of a store's tree pages, which every tree page is read and
/// written under.
///
/// The size bounds how long a page may grow and how large one cell may be: a
/// cell and its slot take less than half of [`Self::cell_capacity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreePageSize(usize);

impl TreePageSize {
    /// The smallest page size, 8 KiB. At this size and above, a cell whose
    /// value is out of line fits whatever else it holds.
    pub const MIN: Self = Self(8_192);
    /// The largest page size, 64 KiB.
    pub const MAX: Self = Self(65_536);
    /// The page size a store uses unless it is told otherwise, 32 KiB.
    pub const DEFAULT: Self = Self(32_768);

    /// Pages of `bytes` bytes.
    pub const fn new(bytes: usize) -> Result<Self, FormatError> {
        if bytes < Self::MIN.0 || bytes > Self::MAX.0 {
            return Err(FormatError::TreePage(TreePageError::PageSizeOutOfRange {
                page_size: bytes,
            }));
        }
        Ok(Self(bytes))
    }

    /// The page size in bytes.
    pub const fn get(self) -> usize {
        self.0
    }

    /// The longest a page of this size may be. Offsets inside a page are 16
    /// bits, so a page of 64 KiB leaves its last byte unused.
    pub const fn image_capacity(self) -> usize {
        if self.0 < TreePage::MAX_LEN {
            self.0
        } else {
            TreePage::MAX_LEN
        }
    }

    /// The bytes of a page that cells and their slots can use.
    pub const fn cell_capacity(self) -> usize {
        self.image_capacity() - TreePage::HEADER_LEN
    }

    /// The longest cell a page of this size takes, without its slot: the
    /// cell and its slot together are smaller than half of
    /// [`Self::cell_capacity`].
    pub const fn max_cell_len(self) -> usize {
        (self.cell_capacity() - 1) / 2 - TreePage::SLOT_LEN
    }

    /// The longest value that can be stored inside a cell whose other parts
    /// take `len_without_value` bytes. A longer value goes out of line.
    pub const fn max_inline_value_len(self, len_without_value: usize) -> usize {
        self.max_cell_len().saturating_sub(len_without_value)
    }

    pub(crate) const fn check_cell_len(self, len: usize) -> Result<(), FormatError> {
        let max = self.max_cell_len();
        if len > max {
            return Err(FormatError::TreePage(TreePageError::CellTooLarge {
                len,
                max,
            }));
        }
        Ok(())
    }
}

impl Default for TreePageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

const _: () = assert!(
    TreeCell::MAX_OUT_OF_LINE_LEN <= TreePageSize::MIN.max_cell_len(),
    "a cell with its value out of line fits in the smallest page"
);
