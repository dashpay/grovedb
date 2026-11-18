//! Reading a tree page.

use std::{iter::FusedIterator, ops::Range};

use super::{Side, TreeCell, TreePageError, TreePageSize};
use crate::{
    error::FormatError,
    header::FormatVersion,
    ids::{CellAddress, PageId, SlotId},
    value_log::ValuePointer,
};

const RESERVED_AT: usize = 1;
const LEN_AT: usize = 2;
const PAGE_ID_AT: usize = 4;
const SLOT_COUNT_AT: usize = 8;
const CELL_COUNT_AT: usize = 10;
const UNUSED_AT: usize = 12;
const DEPARTURES_AT: usize = 14;
const HEADER_LEN: usize = DEPARTURES_AT + 8;
const SLOT_LEN: usize = SlotId::ENCODED_LEN;
const MOST_SLOTS: usize = (TreePage::MAX_LEN - HEADER_LEN) / SLOT_LEN;

fn u16_at(bytes: &[u8], at: usize) -> usize {
    bytes
        .get(at..)
        .and_then(<[u8]>::first_chunk)
        .map_or(0, |field| usize::from(u16::from_le_bytes(*field)))
}

fn le16(value: usize) -> [u8; 2] {
    debug_assert!(value <= TreePage::MAX_LEN);
    (value as u16).to_le_bytes()
}

pub(super) const fn directory_end(slot_count: usize) -> usize {
    HEADER_LEN + slot_count * SLOT_LEN
}

fn slot_of(index: usize) -> SlotId {
    SlotId::from_index(index).unwrap_or(SlotId::MAX)
}

pub(super) fn invalid_slot(index: usize) -> FormatError {
    TreePageError::InvalidSlot {
        slot: slot_of(index),
    }
    .into()
}

/// The fields of a page header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) len: usize,
    pub(super) page: PageId,
    pub(super) slot_count: usize,
    pub(super) cell_count: usize,
    pub(super) unused: usize,
    pub(super) departures: u64,
}

impl Header {
    pub(super) const fn empty(page: PageId, departures: u64) -> Self {
        Self {
            len: HEADER_LEN,
            page,
            slot_count: 0,
            cell_count: 0,
            unused: 0,
            departures,
        }
    }

    pub(super) fn read(bytes: &[u8], version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        let truncated = |needed| FormatError::Truncated {
            needed,
            available: bytes.len(),
        };
        let Some(
            &[kind, reserved, l0, l1, p0, p1, p2, p3, s0, s1, c0, c1, u0, u1, departures @ ..],
        ) = bytes.first_chunk::<HEADER_LEN>()
        else {
            return Err(truncated(HEADER_LEN));
        };
        if kind != TreePage::KIND_CODE {
            return Err(FormatError::UnknownRecordKind {
                code: u32::from(kind),
            });
        }
        if reserved != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        let field = |low, high| usize::from(u16::from_le_bytes([low, high]));
        let len = field(l0, l1);
        if len < HEADER_LEN {
            return Err(TreePageError::PageTooShort { len }.into());
        }
        if len > bytes.len() {
            return Err(truncated(len));
        }
        let page = PageId::decode([p0, p1, p2, p3]).ok_or(FormatError::MissingId)?;
        let slot_count = field(s0, s1);
        let cells_at = directory_end(slot_count);
        if cells_at > len {
            return Err(TreePageError::DirectoryOutOfBounds {
                needed: cells_at,
                len,
            }
            .into());
        }
        Ok(Self {
            len,
            page,
            slot_count,
            cell_count: field(c0, c1),
            unused: field(u0, u1),
            departures: u64::from_le_bytes(departures),
        })
    }

    pub(super) fn write(&self, out: &mut [u8]) {
        out[0] = TreePage::KIND_CODE;
        out[RESERVED_AT] = 0;
        out[LEN_AT..PAGE_ID_AT].copy_from_slice(&le16(self.len));
        out[PAGE_ID_AT..SLOT_COUNT_AT].copy_from_slice(&self.page.encode());
        out[SLOT_COUNT_AT..CELL_COUNT_AT].copy_from_slice(&le16(self.slot_count));
        out[CELL_COUNT_AT..UNUSED_AT].copy_from_slice(&le16(self.cell_count));
        out[UNUSED_AT..DEPARTURES_AT].copy_from_slice(&le16(self.unused));
        out[DEPARTURES_AT..HEADER_LEN].copy_from_slice(&self.departures.to_le_bytes());
    }

    pub(super) const fn cells_at(&self) -> usize {
        directory_end(self.slot_count)
    }

    pub(super) fn check_len(&self, size: TreePageSize) -> Result<(), FormatError> {
        let capacity = size.image_capacity();
        if self.len > capacity {
            return Err(TreePageError::PageTooLong {
                len: self.len,
                capacity,
            }
            .into());
        }
        Ok(())
    }
}

pub(super) fn slot_offset(bytes: &[u8], index: usize) -> usize {
    u16_at(bytes, directory_end(index))
}

pub(super) fn last_slot_is_free(bytes: &[u8], slot_count: usize) -> bool {
    slot_count
        .checked_sub(1)
        .is_some_and(|last| slot_offset(bytes, last) == 0)
}

pub(super) fn set_slot_offset(bytes: &mut [u8], index: usize, offset: usize) {
    let at = directory_end(index);
    bytes[at..at + SLOT_LEN].copy_from_slice(&le16(offset));
}

/// A tree page, borrowed: the cells of Merk nodes under slot numbers that
/// stay the same for as long as a cell is in the page.
///
/// The same type reads an image, which is what a segment stores, and a page
/// that is being changed in a buffer. No accessor allocates.
#[derive(Debug, Clone, Copy)]
pub struct TreePage<'a> {
    bytes: &'a [u8],
    header: Header,
    version: FormatVersion,
}

impl<'a> TreePage<'a> {
    /// The page kind code, the first byte of every tree page.
    pub const KIND_CODE: u8 = 3;
    /// Bytes of the page before the slot directory.
    pub const HEADER_LEN: usize = HEADER_LEN;
    /// Bytes of one entry of the slot directory.
    pub const SLOT_LEN: usize = SLOT_LEN;
    /// The longest page, in bytes. Offsets inside a page are 16 bits.
    pub const MAX_LEN: usize = u16::MAX as usize;

    /// Reads and fully checks the image at the start of `bytes`, written in
    /// format `version` for pages of `size`.
    ///
    /// An image is what a segment stores: a page with its cells in slot
    /// order and no unused byte. It says how long it is, and later bytes
    /// are ignored. It carries no checksum of its own, so `bytes` must come
    /// from a record whose checksum was verified.
    pub fn decode(
        bytes: &'a [u8],
        version: FormatVersion,
        size: TreePageSize,
    ) -> Result<Self, FormatError> {
        let page = Self::from_validated(bytes, version)?;
        page.header.check_len(size)?;
        if page.header.unused != 0 {
            return Err(TreePageError::NotCompact {
                unused: page.header.unused,
            }
            .into());
        }
        let mut next = page.header.cells_at();
        let mut cells = 0;
        for index in 0..page.header.slot_count {
            let offset = slot_offset(page.bytes, index);
            if offset == 0 {
                continue;
            }
            if offset != next {
                return Err(TreePageError::CellsOutOfOrder {
                    slot: slot_of(index),
                }
                .into());
            }
            next += page.checked_cell(index, offset, size)?.encoded_len();
            cells += 1;
        }
        page.check_last_slot()?;
        if next != page.header.len {
            return Err(TreePageError::UnusedBytes {
                used: next,
                len: page.header.len,
            }
            .into());
        }
        page.check_cell_count(cells)?;
        Ok(page)
    }

    /// Borrows a page that was checked before, in constant time: an image
    /// that [`Self::decode`] accepted, or a page in a buffer that was only
    /// changed through [`TreePageMut`](super::TreePageMut).
    ///
    /// Only the header is checked. On other bytes the accessors stay in
    /// bounds and do not panic, but what they return is not meaningful.
    pub fn from_validated(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let header = Header::read(bytes, version)?;
        Ok(Self::with_header(bytes, header, version))
    }

    pub(super) fn with_header(bytes: &'a [u8], header: Header, version: FormatVersion) -> Self {
        Self {
            bytes: bytes.get(..header.len).unwrap_or(bytes),
            header,
            version,
        }
    }

    /// Fully checks a page that may have unused bytes among its cells, as a
    /// page has while it is changed in a buffer: every cell is valid and
    /// small enough for pages of `size`, no two cells share bytes, and the
    /// header's counts are the page's.
    ///
    /// Unlike the accessors, this allocates.
    pub fn check(&self, size: TreePageSize) -> Result<(), FormatError> {
        self.header.check_len(size)?;
        let cells_at = self.header.cells_at();
        let mut extents = Vec::with_capacity(self.header.cell_count);
        for index in 0..self.header.slot_count {
            let offset = slot_offset(self.bytes, index);
            if offset == 0 {
                continue;
            }
            if offset < cells_at {
                return Err(invalid_slot(index));
            }
            let cell = self.checked_cell(index, offset, size)?;
            extents.push((offset, offset + cell.encoded_len(), index));
        }
        self.check_last_slot()?;
        extents.sort_unstable();
        let mut used = 0;
        let mut previous_end = cells_at;
        for &(start, end, index) in &extents {
            if start < previous_end {
                return Err(TreePageError::OverlappingCells {
                    slot: slot_of(index),
                }
                .into());
            }
            used += end - start;
            previous_end = end;
        }
        let unused = self.header.len - cells_at - used;
        if unused != self.header.unused {
            return Err(TreePageError::UnusedBytesMismatch {
                stated: self.header.unused,
                found: unused,
            }
            .into());
        }
        self.check_cell_count(extents.len())
    }

    fn checked_cell(
        &self,
        index: usize,
        offset: usize,
        size: TreePageSize,
    ) -> Result<TreeCell<'a>, FormatError> {
        let bytes = self.bytes.get(offset..).unwrap_or_default();
        let cell = TreeCell::at_start(bytes, self.version).map_err(|error| match error {
            FormatError::Truncated { .. } => invalid_slot(index),
            error => error,
        })?;
        cell.check_hosted_roots()?;
        size.check_cell_len(cell.encoded_len())?;
        Ok(cell)
    }

    fn check_last_slot(&self) -> Result<(), FormatError> {
        if last_slot_is_free(self.bytes, self.header.slot_count) {
            return Err(TreePageError::TrailingFreeSlot.into());
        }
        Ok(())
    }

    fn check_cell_count(&self, found: usize) -> Result<(), FormatError> {
        if found != self.header.cell_count {
            return Err(TreePageError::CellCountMismatch {
                stated: self.header.cell_count,
                found,
            }
            .into());
        }
        Ok(())
    }

    /// The page's bytes, at their exact length.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The format version the page was read under.
    pub const fn version(&self) -> FormatVersion {
        self.version
    }

    /// The page's own id.
    pub const fn id(&self) -> PageId {
        self.header.page
    }

    /// The page's length in bytes. For an image this is the image's length.
    pub const fn encoded_len(&self) -> usize {
        self.header.len
    }

    /// The number of slots in the directory: the highest slot number in use.
    pub const fn slot_count(&self) -> usize {
        self.header.slot_count
    }

    /// The number of cells in the page.
    pub const fn cell_count(&self) -> usize {
        self.header.cell_count
    }

    /// Whether the page holds no cell.
    pub const fn is_empty(&self) -> bool {
        self.header.cell_count == 0
    }

    /// How many cells have left the page since it was created: one for each
    /// cell that was deleted or carried to another page.
    ///
    /// A slot number names the same cell for as long as this count does not
    /// change.
    pub const fn departures(&self) -> u64 {
        self.header.departures
    }

    /// Bytes among the cells that belong to no cell. An image has none.
    pub const fn unused_bytes(&self) -> usize {
        self.header.unused
    }

    /// The bytes a page of `size` still has for cells and their slots, once
    /// its unused bytes are gathered.
    pub const fn free_bytes(&self, size: TreePageSize) -> usize {
        size.image_capacity()
            .saturating_sub(self.header.len)
            .saturating_add(self.header.unused)
    }

    /// The bytes of the page that are in use: the header, the slot directory
    /// and the cells. This is the length of the page's image.
    pub const fn used_bytes(&self) -> usize {
        self.header.len.saturating_sub(self.header.unused)
    }

    /// The cell in `slot`, or `None` if the slot is free.
    pub fn cell(&self, slot: SlotId) -> Option<TreeCell<'a>> {
        self.cell_at(slot.index())
    }

    fn cell_at(&self, index: usize) -> Option<TreeCell<'a>> {
        if index >= self.header.slot_count {
            return None;
        }
        let offset = slot_offset(self.bytes, index);
        if offset < self.header.cells_at() {
            return None;
        }
        TreeCell::at_start(self.bytes.get(offset..)?, self.version).ok()
    }

    #[cfg(test)]
    pub(super) fn cell_range(&self, slot: SlotId) -> Option<Range<usize>> {
        let offset = slot_offset(self.bytes, slot.index());
        Some(offset..offset + self.cell(slot)?.encoded_len())
    }

    /// The cells of the page with their slots, in slot order.
    pub fn cells(&self) -> Cells<'a> {
        Cells {
            page: *self,
            indices: 0..self.header.slot_count,
        }
    }

    /// The fragment-root table: the slots of the cells that are marked as
    /// the root of a fragment, in slot order.
    pub fn fragment_roots(&self) -> impl Iterator<Item = SlotId> + use<'a> {
        self.cells()
            .filter(|(_, cell)| cell.is_fragment_root())
            .map(|(slot, _)| slot)
    }

    /// The lowest slot number that holds no cell. A new cell takes this
    /// slot.
    pub fn lowest_free_slot(&self) -> SlotId {
        let free = (0..self.header.slot_count)
            .find(|&index| slot_offset(self.bytes, index) == 0)
            .unwrap_or(self.header.slot_count);
        slot_of(free)
    }

    /// The slot of the cell whose value is out of line at `pointer`, if the
    /// page has one.
    ///
    /// This is how the owner of a value is found in a tree page: the value's
    /// back pointer names the page, and the pointer names the cell.
    pub fn find_value_pointer(&self, pointer: &ValuePointer) -> Option<SlotId> {
        self.cells()
            .find(|(_, cell)| cell.value_pointer().as_ref() == Some(pointer))
            .map(|(slot, _)| slot)
    }

    /// Appends the page's image to `out`: the header, the slot directory,
    /// and the cells in slot order with no unused byte.
    pub fn write_image(&self, out: &mut Vec<u8>) {
        let start = out.len();
        let cells_at = self.header.cells_at();
        out.reserve(self.used_bytes());
        out.resize(start + cells_at, 0);
        for (slot, cell) in self.cells() {
            let offset = out.len() - start;
            set_slot_offset(&mut out[start..], slot.index(), offset);
            out.extend_from_slice(cell.as_bytes());
        }
        let header = Header {
            len: out.len() - start,
            unused: 0,
            ..self.header
        };
        header.write(&mut out[start..]);
    }

    /// The page's image.
    pub fn to_image(&self) -> Vec<u8> {
        let mut image = Vec::new();
        self.write_image(&mut image);
        image
    }

    /// Checks what the cells of this page say about each other.
    ///
    /// Every link and hosted-root pointer that names a slot of this page
    /// must name a cell, other than the one it is in, and no cell may be
    /// named twice. A cell must be marked as a fragment root exactly when no
    /// cell of the page names it.
    ///
    /// This holds in every committed state. It is not part of
    /// [`Self::decode`], because a page is also read between two changes
    /// that restore it.
    pub fn check_fragments(&self) -> Result<(), FormatError> {
        let mut named = [0u64; MOST_SLOTS.div_ceil(64)];
        let bit = |index: usize| (index / 64, 1u64 << (index % 64));
        for (slot, cell) in self.cells() {
            let children = Side::BOTH
                .into_iter()
                .filter_map(|side| cell.link(side))
                .map(|link| link.child);
            for address in children.chain(cell.hosted_roots().flatten()) {
                if address.page() != self.header.page {
                    continue;
                }
                let index = address.slot().index();
                if address.slot() == slot || self.cell_at(index).is_none() {
                    return Err(TreePageError::DanglingAddress { slot }.into());
                }
                let (word, mask) = bit(index);
                let word = &mut named[word];
                if *word & mask != 0 {
                    return Err(TreePageError::CellNamedTwice {
                        slot: address.slot(),
                    }
                    .into());
                }
                *word |= mask;
            }
        }
        for (slot, cell) in self.cells() {
            let (word, mask) = bit(slot.index());
            let is_named = named[word] & mask != 0;
            if cell.is_fragment_root() == is_named {
                return Err(TreePageError::FragmentRootMismatch { slot }.into());
            }
        }
        Ok(())
    }

    /// The cell a cell address names, if it names one of this page.
    pub fn cell_by_address(&self, address: CellAddress) -> Option<TreeCell<'a>> {
        if address.page() != self.header.page {
            return None;
        }
        self.cell(address.slot())
    }
}

/// The cells of a tree page with their slots, in slot order.
#[derive(Debug, Clone)]
pub struct Cells<'a> {
    page: TreePage<'a>,
    indices: Range<usize>,
}

impl<'a> Cells<'a> {
    fn at(&self, index: usize) -> Option<(SlotId, TreeCell<'a>)> {
        Some((SlotId::from_index(index)?, self.page.cell_at(index)?))
    }
}

impl<'a> Iterator for Cells<'a> {
    type Item = (SlotId, TreeCell<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let index = self.indices.next()?;
            if let Some(cell) = self.at(index) {
                return Some(cell);
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.indices.len()))
    }
}

impl DoubleEndedIterator for Cells<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        loop {
            let index = self.indices.next_back()?;
            if let Some(cell) = self.at(index) {
                return Some(cell);
            }
        }
    }
}

impl FusedIterator for Cells<'_> {}
