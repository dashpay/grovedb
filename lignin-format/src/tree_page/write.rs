//! Changing a tree page where it is.

use super::{
    cell::{self, patch},
    page::{directory_end, invalid_slot, last_slot_is_free, set_slot_offset, slot_offset, Header},
    CellContent, CellValue, Hash, HashField, Side, TreeCell, TreeLink, TreePage, TreePageError,
    TreePageSize,
};
use crate::{
    error::FormatError,
    header::FormatVersion,
    ids::{CellAddress, PageId, SlotId},
};

const SLOT_LEN: usize = TreePage::SLOT_LEN;

/// A tree page in a buffer of its store's page size, open for changes.
///
/// A slot number stays with its cell through every change made here. A hash,
/// a child's address and a fragment-root mark are overwritten where they
/// are. A change that makes a cell longer moves that cell inside the page,
/// and none moves another cell's slot.
///
/// When a call returns [`TreePageError::PageFull`],
/// [`TreePageError::CellTooLarge`], or an error about the slot or the part
/// it was asked to change, the page is as it was before the call.
#[derive(Debug)]
pub struct TreePageMut<'a> {
    buffer: &'a mut [u8],
    size: TreePageSize,
    header: Header,
    version: FormatVersion,
}

impl<'a> TreePageMut<'a> {
    /// Starts an empty page with the id `page` in `buffer`, in the current
    /// format version.
    ///
    /// `departures` is where the page's count of departed cells starts. It
    /// is 0 unless the caller continues the count of an earlier page.
    pub fn init(
        buffer: &'a mut [u8],
        size: TreePageSize,
        page: PageId,
        departures: u64,
    ) -> Result<Self, FormatError> {
        Self::check_buffer(buffer, size)?;
        let mut page = Self {
            buffer,
            size,
            header: Header::empty(page, departures),
            version: FormatVersion::CURRENT,
        };
        page.store_header();
        Ok(page)
    }

    /// Opens the page at the start of `buffer`, in constant time: an image
    /// that [`TreePage::decode`] accepted and that was copied there, or a
    /// page that was changed there before.
    pub fn open(
        buffer: &'a mut [u8],
        size: TreePageSize,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        Self::check_buffer(buffer, size)?;
        let header = Header::read(buffer, version)?;
        header.check_len(size)?;
        Ok(Self {
            buffer,
            size,
            header,
            version,
        })
    }

    fn check_buffer(buffer: &[u8], size: TreePageSize) -> Result<(), FormatError> {
        let needed = size.image_capacity();
        if buffer.len() < needed {
            return Err(TreePageError::BufferTooSmall {
                len: buffer.len(),
                needed,
            }
            .into());
        }
        Ok(())
    }

    /// The page as it is now, for reading.
    pub fn page(&self) -> TreePage<'_> {
        TreePage::with_header(self.buffer, self.header, self.version)
    }

    /// The bytes the page still has for cells and their slots.
    pub const fn free_bytes(&self) -> usize {
        self.size.image_capacity() - self.header.len + self.header.unused
    }

    /// Puts a new cell into the free slot `slot`.
    pub fn insert(&mut self, slot: SlotId, cell: &CellContent<'_>) -> Result<(), FormatError> {
        let len = cell.checked_len()?;
        let offset = self.allocate(slot, len)?;
        cell.write(&mut self.buffer[offset..offset + len]);
        Ok(())
    }

    /// Puts a cell that was read from a page, or decoded from its own
    /// bytes, into the free slot `slot`, as it is. This is how a cell
    /// arrives that was carried from another page.
    pub fn insert_encoded(&mut self, slot: SlotId, cell: TreeCell<'_>) -> Result<(), FormatError> {
        cell.check_hosted_roots()?;
        let bytes = cell.as_bytes();
        let offset = self.allocate(slot, bytes.len())?;
        self.buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Replaces the cell in `slot` by another cell of the same node: one
    /// with the same key. The slot keeps its number, and no cell counts as
    /// having left the page.
    ///
    /// A cell with another key is refused. Another node takes a slot only
    /// through [`Self::remove`] and [`Self::insert`], so that the count of
    /// departures tells.
    pub fn replace(&mut self, slot: SlotId, cell: &CellContent<'_>) -> Result<(), FormatError> {
        self.size.check_cell_len(cell.checked_len()?)?;
        self.check_same_key(slot, cell.key)?;
        let bytes = cell.encode()?;
        self.replace_bytes(slot, &bytes)
    }

    /// Replaces the cell in `slot` by a cell that was read from a page or
    /// decoded from its own bytes, as [`Self::replace`] does.
    pub fn replace_encoded(&mut self, slot: SlotId, cell: TreeCell<'_>) -> Result<(), FormatError> {
        cell.check_hosted_roots()?;
        self.size.check_cell_len(cell.encoded_len())?;
        self.check_same_key(slot, cell.key())?;
        self.replace_bytes(slot, cell.as_bytes())
    }

    fn check_same_key(&self, slot: SlotId, key: &[u8]) -> Result<(), FormatError> {
        let (_, cell) = self.occupied(slot)?;
        if cell.key() != key {
            return Err(TreePageError::KeyChanged { slot }.into());
        }
        Ok(())
    }

    /// Takes the cell in `slot` out of the page: the node was deleted, or
    /// its cell was carried to another page. The slot is free afterwards,
    /// and the page's count of departures goes up by one.
    pub fn remove(&mut self, slot: SlotId) -> Result<(), FormatError> {
        let (offset, len) = self
            .occupied(slot)
            .map(|(offset, cell)| (offset, cell.encoded_len()))?;
        set_slot_offset(self.buffer, slot.index(), 0);
        self.header.cell_count -= 1;
        self.header.departures = self.header.departures.wrapping_add(1);
        self.release(offset, len);
        if self.header.cell_count == 0 {
            self.header = Header::empty(self.header.page, self.header.departures);
        }
        while last_slot_is_free(self.buffer, self.header.slot_count) {
            self.header.slot_count -= 1;
            self.header.unused += SLOT_LEN;
        }
        self.store_header();
        Ok(())
    }

    /// Overwrites one of the hashes of the cell in `slot`, where it is.
    /// Nothing else in the page changes.
    pub fn set_hash(
        &mut self,
        slot: SlotId,
        field: HashField,
        hash: &Hash,
    ) -> Result<(), FormatError> {
        let (offset, cell) = self.occupied(slot)?;
        let at = offset + cell.hash_at(field).map_err(|side| no_link(slot, side))?;
        self.buffer[at..at + hash.len()].copy_from_slice(hash);
        Ok(())
    }

    /// Overwrites the address of a child in the link of the cell in `slot`,
    /// where it is: the child's cell was carried to another page, or another
    /// cell took its place.
    pub fn set_child(
        &mut self,
        slot: SlotId,
        side: Side,
        child: CellAddress,
    ) -> Result<(), FormatError> {
        let (offset, cell) = self.occupied(slot)?;
        if !cell.has_link(side) {
            return Err(no_link(slot, side));
        }
        let at = offset + cell.link_extent(side).0;
        self.buffer[at..at + CellAddress::ENCODED_LEN].copy_from_slice(&child.encode());
        Ok(())
    }

    /// Sets, replaces or removes the link on a side of the cell in `slot`.
    ///
    /// A link that replaces one with an aggregate of the same length is
    /// overwritten where it is. Otherwise the cell changes its length and
    /// may move inside the page.
    pub fn set_link(
        &mut self,
        slot: SlotId,
        side: Side,
        link: Option<&TreeLink<'_>>,
    ) -> Result<(), FormatError> {
        let mut buffer = [0; TreeLink::MAX_LEN];
        let bytes = match link {
            Some(link) => link.checked_write(&mut buffer)?,
            None => &[],
        };
        self.splice(
            slot,
            |cell| cell.link_extent(side),
            bytes,
            |cell| patch::set_link_present(cell, side, link.is_some()),
        )
    }

    /// Marks the cell in `slot` as the root of a fragment, or takes the mark
    /// off, where the cell is.
    pub fn set_fragment_root(
        &mut self,
        slot: SlotId,
        fragment_root: bool,
    ) -> Result<(), FormatError> {
        let (offset, cell) = self.occupied(slot)?;
        let end = offset + cell.encoded_len();
        patch::set_fragment_root(&mut self.buffer[offset..end], fragment_root);
        Ok(())
    }

    /// Overwrites the hosted-root pointer at position `index` of the cell in
    /// `slot`, where it is.
    pub fn set_hosted_root(
        &mut self,
        slot: SlotId,
        index: usize,
        root: Option<CellAddress>,
    ) -> Result<(), FormatError> {
        let (offset, cell) = self.occupied(slot)?;
        if index >= cell.hosted_root_count() {
            return Err(TreePageError::NoSuchHostedRoot { slot, index }.into());
        }
        let at = offset + cell.hosted_roots_extent().0 + index * TreeCell::HOSTED_ROOT_LEN;
        self.buffer[at..at + TreeCell::HOSTED_ROOT_LEN]
            .copy_from_slice(&CellAddress::encode_optional(root));
        Ok(())
    }

    /// Replaces all hosted-root pointers of the cell in `slot`. The cell
    /// changes its length if their number changes.
    pub fn set_hosted_roots(
        &mut self,
        slot: SlotId,
        roots: &[Option<CellAddress>],
    ) -> Result<(), FormatError> {
        let mut buffer = [0; TreeCell::MAX_HOSTED_ROOTS * TreeCell::HOSTED_ROOT_LEN];
        let bytes = cell::hosted_root_bytes(roots, &mut buffer)?;
        self.splice(
            slot,
            |cell| cell.hosted_roots_extent(),
            bytes,
            |cell| patch::set_hosted_root_count(cell, roots.len()),
        )
    }

    /// Replaces the feature of the cell in `slot`. A feature of the same
    /// length is overwritten where it is.
    pub fn set_feature(&mut self, slot: SlotId, feature: &[u8]) -> Result<(), FormatError> {
        cell::check_feature(feature)?;
        self.splice(
            slot,
            |cell| cell.feature_extent(),
            feature,
            |cell| patch::set_feature_len(cell, feature.len()),
        )
    }

    /// Replaces the value of the cell in `slot`, by a value or by a pointer
    /// to one. The hashes and links of the cell keep their bytes.
    ///
    /// A value that takes the same room in the cell is overwritten where it
    /// is: a value of the same length, or another pointer in place of a
    /// pointer.
    pub fn set_value(&mut self, slot: SlotId, value: CellValue<'_>) -> Result<(), FormatError> {
        let mut location = [0; TreeCell::VALUE_LOCATION_LEN];
        let (bytes, len) = cell::stored_value(&value, &mut location)?;
        self.splice(
            slot,
            |cell| cell.value_extent(),
            bytes,
            |cell| patch::set_value_header(cell, &value, len),
        )
    }

    /// Gathers the unused bytes of the page: afterwards its cells are in
    /// slot order with nothing between them, and its bytes are its image.
    pub fn compact(&mut self) -> Result<(), FormatError> {
        self.repack(self.header.slot_count)?;
        self.store_header();
        Ok(())
    }

    fn store_header(&mut self) {
        self.header.write(self.buffer);
    }

    fn occupied(&self, slot: SlotId) -> Result<(usize, TreeCell<'_>), FormatError> {
        let index = slot.index();
        let offset = if index < self.header.slot_count {
            slot_offset(self.buffer, index)
        } else {
            0
        };
        if offset == 0 {
            return Err(TreePageError::SlotFree { slot }.into());
        }
        Ok((offset, self.cell_at(index, offset)?))
    }

    fn cell_at(&self, index: usize, offset: usize) -> Result<TreeCell<'_>, FormatError> {
        let cells = self.header.cells_at()..self.header.len;
        let bytes = self.buffer.get(offset..cells.end).unwrap_or_default();
        if !cells.contains(&offset) {
            return Err(invalid_slot(index));
        }
        TreeCell::at_start(bytes, self.version).map_err(|_| invalid_slot(index))
    }

    fn release(&mut self, offset: usize, len: usize) {
        if offset + len == self.header.len {
            self.header.len -= len;
        } else {
            self.header.unused += len;
        }
    }

    fn allocate(&mut self, slot: SlotId, len: usize) -> Result<usize, FormatError> {
        self.size.check_cell_len(len)?;
        let index = slot.index();
        if index < self.header.slot_count && slot_offset(self.buffer, index) != 0 {
            return Err(TreePageError::SlotOccupied { slot }.into());
        }
        let slot_count = self.header.slot_count.max(index + 1);
        let needed = len + (slot_count - self.header.slot_count) * SLOT_LEN;
        let available = self.free_bytes();
        if needed > available {
            return Err(TreePageError::PageFull { needed, available }.into());
        }
        if slot_count > self.header.slot_count {
            self.grow_directory(slot_count)?;
        }
        let offset = self.take_tail(len)?;
        set_slot_offset(self.buffer, index, offset);
        self.header.cell_count += 1;
        self.store_header();
        Ok(offset)
    }

    fn lowest_cell(&self) -> Option<(usize, usize)> {
        (0..self.header.slot_count)
            .map(|index| (slot_offset(self.buffer, index), index))
            .filter(|&(offset, _)| offset != 0)
            .min()
    }

    fn grow_directory(&mut self, slot_count: usize) -> Result<(), FormatError> {
        let old_cells_at = self.header.cells_at();
        let cells_at = directory_end(slot_count);
        let cell_bytes = self.header.len - old_cells_at - self.header.unused;
        while let Some((offset, index)) = self.lowest_cell().filter(|&(at, _)| at < cells_at) {
            let len = self.cell_at(index, offset)?.encoded_len();
            let target = self.header.len.max(cells_at);
            if target + len > self.size.image_capacity() {
                return self.repack(slot_count);
            }
            self.buffer.copy_within(offset..offset + len, target);
            set_slot_offset(self.buffer, index, target);
            self.header.len = target + len;
        }
        self.buffer[old_cells_at..cells_at].fill(0);
        self.header.slot_count = slot_count;
        self.header.len = self.header.len.max(cells_at);
        self.header.unused = self.header.len - cells_at - cell_bytes;
        Ok(())
    }

    fn take_tail(&mut self, len: usize) -> Result<usize, FormatError> {
        if self.header.len + len > self.size.image_capacity() {
            self.repack(self.header.slot_count)?;
        }
        let offset = self.header.len;
        self.header.len += len;
        Ok(offset)
    }

    fn repack(&mut self, slot_count: usize) -> Result<(), FormatError> {
        let old_cells_at = self.header.cells_at();
        let cells = self.buffer[old_cells_at..self.header.len].to_vec();
        let mut next = directory_end(slot_count);
        self.buffer[old_cells_at..next].fill(0);
        for index in 0..self.header.slot_count {
            let offset = slot_offset(self.buffer, index);
            if offset == 0 {
                continue;
            }
            let cell = offset
                .checked_sub(old_cells_at)
                .and_then(|at| cells.get(at..))
                .and_then(|bytes| TreeCell::at_start(bytes, self.version).ok())
                .ok_or_else(|| invalid_slot(index))?;
            let end = next + cell.encoded_len();
            self.buffer
                .get_mut(next..end)
                .ok_or_else(|| invalid_slot(index))?
                .copy_from_slice(cell.as_bytes());
            set_slot_offset(self.buffer, index, next);
            next = end;
        }
        self.header.slot_count = slot_count;
        self.header.len = next;
        self.header.unused = 0;
        Ok(())
    }

    fn replace_bytes(&mut self, slot: SlotId, new: &[u8]) -> Result<(), FormatError> {
        let (offset, old_len) = self
            .occupied(slot)
            .map(|(offset, cell)| (offset, cell.encoded_len()))?;
        self.size.check_cell_len(new.len())?;
        let index = slot.index();
        if new.len() <= old_len {
            self.buffer[offset..offset + new.len()].copy_from_slice(new);
            self.release(offset + new.len(), old_len - new.len());
        } else {
            let needed = new.len() - old_len;
            let available = self.free_bytes();
            if needed > available {
                return Err(TreePageError::PageFull { needed, available }.into());
            }
            set_slot_offset(self.buffer, index, 0);
            self.release(offset, old_len);
            let offset = self.take_tail(new.len())?;
            self.buffer[offset..offset + new.len()].copy_from_slice(new);
            set_slot_offset(self.buffer, index, offset);
        }
        self.store_header();
        Ok(())
    }

    fn splice(
        &mut self,
        slot: SlotId,
        extent: impl FnOnce(&TreeCell<'_>) -> (usize, usize),
        replacement: &[u8],
        patch_header: impl FnOnce(&mut [u8]),
    ) -> Result<(), FormatError> {
        let (offset, cell) = self.occupied(slot)?;
        let (start, end) = extent(&cell);
        let old = cell.as_bytes();
        if end - start == replacement.len() {
            let cell_end = offset + old.len();
            self.buffer[offset + start..offset + end].copy_from_slice(replacement);
            patch_header(&mut self.buffer[offset..cell_end]);
            return Ok(());
        }
        let mut new = Vec::with_capacity(old.len() - (end - start) + replacement.len());
        new.extend_from_slice(&old[..start]);
        new.extend_from_slice(replacement);
        new.extend_from_slice(&old[end..]);
        patch_header(&mut new);
        self.replace_bytes(slot, &new)
    }
}

fn no_link(slot: SlotId, side: Side) -> FormatError {
    TreePageError::NoSuchLink { slot, side }.into()
}
