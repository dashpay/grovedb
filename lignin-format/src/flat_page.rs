//! Flat pages: B+tree leaf and branch page images with prefix-compressed
//! keys, for ordered data that is not part of a Merk tree.
//!
//! See `docs/lignindb/flat-page.md`.
//!
//! ```text
//! offset  size  field
//!      0     1  page kind: 1 leaf, 2 branch
//!      1     1  level: 0 in a leaf, 1 to 255 in a branch
//!      2     2  image length
//!      4     4  checksum of bytes 0..4 followed by bytes 8..length
//!      8     2  entry count
//!     10     2  prefix length
//!     12     4  first child page id (branch only)
//!   then        the prefix, the slot directory, the cells
//! ```

use std::{cmp::Ordering, fmt, ops::Range};

use crate::{checksum::Checksum, error::FormatError, header::FormatVersion, ids::PageId};

const COMMON_HEADER_LEN: usize = 8;
const LEN_AT: usize = 2;
const CHECKSUM_AT: usize = 4;
const COUNT_AT: usize = 8;
const PREFIX_LEN_AT: usize = 10;
const FIRST_CHILD_AT: usize = 12;
const OUT_OF_LINE: u8 = 1;

/// Why a byte string is not a flat page image, or why a page cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatPageError {
    /// The image length field is smaller than the page's own header.
    ImageTooShort {
        /// The image length.
        len: usize,
        /// The header length of the page kind.
        header_len: usize,
    },
    /// The level is not 0 in a leaf, or is 0 in a branch.
    InvalidLevel {
        /// The page kind.
        kind: FlatPageKind,
        /// The level found.
        level: u8,
    },
    /// The prefix and the slot directory do not fit in the image.
    DirectoryOutOfBounds {
        /// The offset at which the cells would start.
        needed: usize,
        /// The image length.
        len: usize,
    },
    /// A slot's cell ends before it starts or after the image does, or is
    /// too short for what the slot says it holds.
    InvalidSlot {
        /// The slot's position.
        slot: usize,
    },
    /// A key is not greater than the key before it.
    KeysOutOfOrder {
        /// The position of the key that is out of order.
        slot: usize,
    },
    /// The last cell ends before the image does.
    UnusedBytes {
        /// The offset at which the cells end.
        used: usize,
        /// The image length.
        len: usize,
    },
    /// A page was asked for with a capacity above [`FlatPage::MAX_LEN`].
    CapacityOutOfRange {
        /// The capacity asked for.
        capacity: usize,
    },
    /// A key does not start with the page's prefix.
    KeyOutsidePrefix,
    /// The page has no room for what was added. The page is unchanged.
    PageFull {
        /// The image length the addition would give.
        needed: usize,
        /// The capacity the page was created with.
        capacity: usize,
    },
}

impl fmt::Display for FlatPageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImageTooShort { len, header_len } => write!(
                f,
                "image length {len} is below the {header_len}-byte header"
            ),
            Self::InvalidLevel { kind, level } => write!(f, "{kind:?} page with level {level}"),
            Self::DirectoryOutOfBounds { needed, len } => write!(
                f,
                "prefix and slot directory end at {needed}, past the image length {len}"
            ),
            Self::InvalidSlot { slot } => write!(f, "slot {slot} does not describe a cell"),
            Self::KeysOutOfOrder { slot } => {
                write!(f, "key {slot} is not greater than the key before it")
            }
            Self::UnusedBytes { used, len } => {
                write!(f, "cells end at {used}, before the image length {len}")
            }
            Self::CapacityOutOfRange { capacity } => write!(
                f,
                "capacity {capacity} exceeds the maximum of {}",
                FlatPage::MAX_LEN
            ),
            Self::KeyOutsidePrefix => write!(f, "key does not start with the page prefix"),
            Self::PageFull { needed, capacity } => {
                write!(f, "page full: need {needed} bytes, capacity is {capacity}")
            }
        }
    }
}

impl From<FlatPageError> for FormatError {
    fn from(error: FlatPageError) -> Self {
        Self::FlatPage(error)
    }
}

/// Whether a flat page holds values or children. The code is the first byte
/// of the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FlatPageKind {
    /// Keys with their values.
    Leaf = 1,
    /// Separator keys with the pages below them.
    Branch = 2,
}

impl FlatPageKind {
    /// Every flat page kind of the current format version.
    pub const ALL: [Self; 2] = [Self::Leaf, Self::Branch];

    /// The kind with this code in a file of format `version`.
    pub const fn from_code(code: u8, version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => match code {
                1 => Ok(Self::Leaf),
                2 => Ok(Self::Branch),
                code => Err(FormatError::UnknownRecordKind { code: code as u32 }),
            },
        }
    }

    /// The code stored in the image.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Bytes of the image before the prefix.
    pub const fn header_len(self) -> usize {
        match self {
            Self::Leaf => 12,
            Self::Branch => 16,
        }
    }

    /// Bytes an entry takes on top of its key suffix and, in a leaf, its
    /// value.
    pub const fn entry_overhead(self) -> usize {
        match self {
            Self::Leaf => self.slot_len(),
            Self::Branch => self.slot_len() + PageId::ENCODED_LEN,
        }
    }

    const fn slot_len(self) -> usize {
        match self {
            Self::Leaf => 5,
            Self::Branch => 2,
        }
    }
}

/// A key in a flat page: the page's prefix followed by the key's own suffix.
///
/// The two parts are borrowed from the image. Two keys are equal when their
/// bytes are, wherever each page splits them.
#[derive(Debug, Clone, Copy)]
pub struct FlatKey<'a> {
    prefix: &'a [u8],
    suffix: &'a [u8],
}

impl<'a> FlatKey<'a> {
    /// The leading bytes, which every key of the page shares.
    pub const fn prefix(&self) -> &'a [u8] {
        self.prefix
    }

    /// The bytes after the prefix.
    pub const fn suffix(&self) -> &'a [u8] {
        self.suffix
    }

    /// The key's length in bytes.
    pub const fn len(&self) -> usize {
        self.prefix.len() + self.suffix.len()
    }

    /// Whether the key is the empty byte string.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Compares the key with `other`, byte by byte.
    pub fn cmp_bytes(&self, other: &[u8]) -> Ordering {
        let (head, tail) = other.split_at(self.prefix.len().min(other.len()));
        self.prefix.cmp(head).then_with(|| self.suffix.cmp(tail))
    }

    /// Appends the key's bytes to `out`.
    pub fn append_to(&self, out: &mut Vec<u8>) {
        out.reserve(self.len());
        out.extend_from_slice(self.prefix);
        out.extend_from_slice(self.suffix);
    }

    /// The key's bytes in one allocation.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut key = Vec::new();
        self.append_to(&mut key);
        key
    }

    fn bytes(&self) -> impl Iterator<Item = &'a u8> + use<'a> {
        self.prefix.iter().chain(self.suffix)
    }
}

impl PartialEq for FlatKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.bytes().eq(other.bytes())
    }
}

impl Eq for FlatKey<'_> {}

impl PartialOrd for FlatKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FlatKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}

impl PartialEq<[u8]> for FlatKey<'_> {
    fn eq(&self, other: &[u8]) -> bool {
        self.cmp_bytes(other) == Ordering::Equal
    }
}

/// What a leaf stores for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatValue<'a> {
    /// The value itself.
    Inline(&'a [u8]),
    /// A pointer to a value stored outside the page. The page does not
    /// define or check the pointer's layout.
    OutOfLine(&'a [u8]),
}

impl<'a> FlatValue<'a> {
    /// The bytes stored in the page: the value, or the pointer to it.
    pub const fn bytes(&self) -> &'a [u8] {
        match *self {
            Self::Inline(bytes) | Self::OutOfLine(bytes) => bytes,
        }
    }

    /// Whether the stored bytes are the value itself.
    pub const fn is_inline(&self) -> bool {
        matches!(self, Self::Inline(_))
    }

    const fn flags(&self) -> u8 {
        match self {
            Self::Inline(_) => 0,
            Self::OutOfLine(_) => OUT_OF_LINE,
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<usize> {
    let field: &[u8; 2] = bytes.get(at..)?.first_chunk()?;
    Some(usize::from(u16::from_le_bytes(*field)))
}

fn page_id_at(bytes: &[u8], at: usize) -> Option<PageId> {
    PageId::decode(*bytes.get(at..)?.first_chunk()?)
}

fn le16(value: usize) -> [u8; 2] {
    debug_assert!(value <= FlatPage::MAX_LEN);
    (value as u16).to_le_bytes()
}

fn image_at_start(bytes: &[u8]) -> Result<&[u8], FormatError> {
    let truncated = |needed| FormatError::Truncated {
        needed,
        available: bytes.len(),
    };
    let Some(&[_, _, len_low, len_high, ..]) = bytes.first_chunk::<COMMON_HEADER_LEN>() else {
        return Err(truncated(COMMON_HEADER_LEN));
    };
    let len = usize::from(u16::from_le_bytes([len_low, len_high]));
    let header_len = FlatPageKind::Leaf.header_len();
    if len < header_len {
        return Err(FlatPageError::ImageTooShort { len, header_len }.into());
    }
    bytes.get(..len).ok_or(truncated(len))
}

fn verify_checksum(image: &[u8]) -> Result<(), FormatError> {
    let Some((&[kind, level, len_low, len_high, stored @ ..], body)) =
        image.split_first_chunk::<COMMON_HEADER_LEN>()
    else {
        return Err(FormatError::Truncated {
            needed: COMMON_HEADER_LEN,
            available: image.len(),
        });
    };
    Checksum::decode(stored).verify(&[&[kind, level, len_low, len_high], body])
}

#[derive(Debug, Clone, Copy)]
struct View<'a> {
    image: &'a [u8],
    kind: FlatPageKind,
    level: u8,
    prefix: &'a [u8],
    slots: &'a [u8],
    count: usize,
    cells_at: usize,
}

impl<'a> View<'a> {
    fn open(image: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let too_short = |header_len| FlatPageError::ImageTooShort {
            len: image.len(),
            header_len,
        };
        let &[code, level] = image.first_chunk().ok_or(too_short(2))?;
        let kind = FlatPageKind::from_code(code, version)?;
        if (level == 0) != (kind == FlatPageKind::Leaf) {
            return Err(FlatPageError::InvalidLevel { kind, level }.into());
        }
        let header_len = kind.header_len();
        let header = image.get(..header_len).ok_or(too_short(header_len))?;
        let (Some(count), Some(prefix_len)) =
            (u16_at(header, COUNT_AT), u16_at(header, PREFIX_LEN_AT))
        else {
            return Err(too_short(header_len).into());
        };
        if kind == FlatPageKind::Branch && page_id_at(header, FIRST_CHILD_AT).is_none() {
            return Err(FormatError::MissingId);
        }
        let slots_at = header_len + prefix_len;
        let cells_at = slots_at + count * kind.slot_len();
        let (Some(prefix), Some(slots)) = (
            image.get(header_len..slots_at),
            image.get(slots_at..cells_at),
        ) else {
            return Err(FlatPageError::DirectoryOutOfBounds {
                needed: cells_at,
                len: image.len(),
            }
            .into());
        };
        Ok(Self {
            image,
            kind,
            level,
            prefix,
            slots,
            count,
            cells_at,
        })
    }

    fn validate(&self) -> Result<(), FormatError> {
        let mut start = self.cells_at;
        let mut previous: Option<&[u8]> = None;
        for slot in 0..self.count {
            let invalid = FlatPageError::InvalidSlot { slot };
            let end = self.cell_end(slot).ok_or(invalid)?;
            let cell = self.image.get(start..end).ok_or(invalid)?;
            let suffix = match self.kind {
                FlatPageKind::Leaf => {
                    let suffix = self
                        .key_len(slot)
                        .and_then(|key_len| cell.get(..key_len))
                        .ok_or(invalid)?;
                    if self
                        .flags(slot)
                        .is_none_or(|flags| flags & !OUT_OF_LINE != 0)
                    {
                        return Err(FormatError::NonZeroReserved);
                    }
                    suffix
                }
                FlatPageKind::Branch => {
                    let (suffix, child) = cell.split_last_chunk().ok_or(invalid)?;
                    PageId::decode(*child).ok_or(FormatError::MissingId)?;
                    suffix
                }
            };
            if previous.is_some_and(|previous| previous >= suffix) {
                return Err(FlatPageError::KeysOutOfOrder { slot }.into());
            }
            previous = Some(suffix);
            start = end;
        }
        if start != self.image.len() {
            return Err(FlatPageError::UnusedBytes {
                used: start,
                len: self.image.len(),
            }
            .into());
        }
        Ok(())
    }

    fn slot(&self, slot: usize) -> Option<&'a [u8]> {
        let len = self.kind.slot_len();
        let at = slot.checked_mul(len)?;
        self.slots.get(at..at.checked_add(len)?)
    }

    fn cell_end(&self, slot: usize) -> Option<usize> {
        u16_at(self.slot(slot)?, 0)
    }

    fn key_len(&self, slot: usize) -> Option<usize> {
        u16_at(self.slot(slot)?, 2)
    }

    fn flags(&self, slot: usize) -> Option<u8> {
        self.slot(slot)?.get(4).copied()
    }

    fn cell(&self, slot: usize) -> Option<&'a [u8]> {
        let start = match slot.checked_sub(1) {
            Some(before) => self.cell_end(before)?,
            None => self.cells_at,
        };
        self.image.get(start..self.cell_end(slot)?)
    }

    fn split_cell(&self, slot: usize) -> Option<(&'a [u8], &'a [u8])> {
        let cell = self.cell(slot)?;
        let key_len = match self.kind {
            FlatPageKind::Leaf => self.key_len(slot)?,
            FlatPageKind::Branch => cell.len().checked_sub(PageId::ENCODED_LEN)?,
        };
        cell.split_at_checked(key_len)
    }

    fn key(&self, slot: usize) -> Option<FlatKey<'a>> {
        let (suffix, _) = self.split_cell(slot)?;
        Some(FlatKey {
            prefix: self.prefix,
            suffix,
        })
    }

    fn search(&self, key: &[u8]) -> Result<usize, usize> {
        let count = self.count;
        let needle = match key.split_at_checked(self.prefix.len()) {
            Some((head, tail)) => match head.cmp(self.prefix) {
                Ordering::Less => return Err(0),
                Ordering::Greater => return Err(count),
                Ordering::Equal => tail,
            },
            None => return Err(if key > self.prefix { count } else { 0 }),
        };
        let (mut low, mut high) = (0, count);
        while low < high {
            let middle = low + (high - low) / 2;
            let Some((suffix, _)) = self.split_cell(middle) else {
                return Err(low);
            };
            match suffix.cmp(needle) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Ok(middle),
            }
        }
        Err(low)
    }
}

/// A flat page image, borrowed.
#[derive(Debug, Clone, Copy)]
pub enum FlatPage<'a> {
    /// A page of keys and values.
    Leaf(FlatLeaf<'a>),
    /// A page of separator keys and children.
    Branch(FlatBranch<'a>),
}

impl<'a> FlatPage<'a> {
    /// The longest image, in bytes. Offsets inside a page are 16 bits.
    pub const MAX_LEN: usize = u16::MAX as usize;

    /// Reads and fully checks the image at the start of `bytes`, written in
    /// format `version`.
    ///
    /// The image says how long it is, and later bytes are ignored, so `bytes`
    /// may be a whole frame. The checks run in a fixed order: length,
    /// checksum, kind, level, header, directory, then each slot in turn.
    pub fn decode(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let image = image_at_start(bytes)?;
        verify_checksum(image)?;
        let view = View::open(image, version)?;
        view.validate()?;
        Ok(Self::from_view(view))
    }

    /// Borrows an image that [`Self::decode`] has already accepted, in
    /// constant time.
    ///
    /// Only the header is checked: not the checksum, the slots or the key
    /// order. On bytes `decode` would reject, the accessors stay in bounds
    /// and do not panic, but what they return is not meaningful.
    pub fn from_validated(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        View::open(image_at_start(bytes)?, version).map(Self::from_view)
    }

    fn from_view(view: View<'a>) -> Self {
        match view.kind {
            FlatPageKind::Leaf => Self::Leaf(FlatLeaf(view)),
            FlatPageKind::Branch => Self::Branch(FlatBranch(view)),
        }
    }

    const fn view(&self) -> &View<'a> {
        match self {
            Self::Leaf(FlatLeaf(view)) | Self::Branch(FlatBranch(view)) => view,
        }
    }

    /// Whether the page is a leaf or a branch.
    pub const fn kind(&self) -> FlatPageKind {
        self.view().kind
    }

    /// The page's distance from the leaves: 0 for a leaf.
    pub const fn level(&self) -> u8 {
        self.view().level
    }

    /// The image, at its exact length.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.view().image
    }

    /// The bytes every key of the page starts with.
    pub const fn prefix(&self) -> &'a [u8] {
        self.view().prefix
    }

    /// The number of keys in the page.
    pub const fn len(&self) -> usize {
        self.view().count
    }

    /// Whether the page has no key.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The page as a leaf, if it is one.
    pub const fn leaf(self) -> Option<FlatLeaf<'a>> {
        match self {
            Self::Leaf(leaf) => Some(leaf),
            Self::Branch(_) => None,
        }
    }

    /// The page as a branch, if it is one.
    pub const fn branch(self) -> Option<FlatBranch<'a>> {
        match self {
            Self::Leaf(_) => None,
            Self::Branch(branch) => Some(branch),
        }
    }
}

/// A leaf page image, borrowed: keys in ascending order, each with a value.
///
/// No accessor allocates.
#[derive(Debug, Clone, Copy)]
pub struct FlatLeaf<'a>(View<'a>);

impl<'a> FlatLeaf<'a> {
    /// Bytes of the image before the prefix.
    pub const HEADER_LEN: usize = FlatPageKind::Leaf.header_len();
    /// Bytes an entry takes on top of its key suffix and value.
    pub const ENTRY_OVERHEAD: usize = FlatPageKind::Leaf.entry_overhead();

    /// Bytes an entry adds to the image.
    pub const fn entry_len(key_suffix_len: usize, value_len: usize) -> usize {
        Self::ENTRY_OVERHEAD + key_suffix_len + value_len
    }

    /// The image, at its exact length.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.0.image
    }

    /// The bytes every key of the page starts with.
    pub const fn prefix(&self) -> &'a [u8] {
        self.0.prefix
    }

    /// The number of entries.
    pub const fn len(&self) -> usize {
        self.0.count
    }

    /// Whether the leaf has no entry.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The key at position `slot`, counting from 0 in key order.
    pub fn key(&self, slot: usize) -> Option<FlatKey<'a>> {
        self.0.key(slot)
    }

    /// The value at position `slot`.
    pub fn value(&self, slot: usize) -> Option<FlatValue<'a>> {
        let (_, value) = self.0.split_cell(slot)?;
        Some(if self.0.flags(slot)? & OUT_OF_LINE == 0 {
            FlatValue::Inline(value)
        } else {
            FlatValue::OutOfLine(value)
        })
    }

    /// The key and value at position `slot`.
    pub fn entry(&self, slot: usize) -> Option<(FlatKey<'a>, FlatValue<'a>)> {
        Some((self.key(slot)?, self.value(slot)?))
    }

    /// Binary search for `key`.
    ///
    /// `Ok` holds the position of the entry with that key. `Err` holds the
    /// position at which the key would be inserted: the number of entries
    /// with a smaller key.
    pub fn search(&self, key: &[u8]) -> Result<usize, usize> {
        self.0.search(key)
    }

    /// The value stored for `key`.
    pub fn get(&self, key: &[u8]) -> Option<FlatValue<'a>> {
        self.value(self.search(key).ok()?)
    }

    /// The entries in key order.
    pub fn iter(&self) -> FlatLeafIter<'a> {
        FlatLeafIter {
            leaf: *self,
            slots: 0..self.len(),
        }
    }
}

impl<'a> IntoIterator for FlatLeaf<'a> {
    type IntoIter = FlatLeafIter<'a>;
    type Item = (FlatKey<'a>, FlatValue<'a>);

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The entries of a leaf, in key order.
#[derive(Debug, Clone)]
pub struct FlatLeafIter<'a> {
    leaf: FlatLeaf<'a>,
    slots: Range<usize>,
}

impl<'a> Iterator for FlatLeafIter<'a> {
    type Item = (FlatKey<'a>, FlatValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        self.leaf.entry(self.slots.next()?)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.slots.size_hint()
    }
}

impl DoubleEndedIterator for FlatLeafIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.leaf.entry(self.slots.next_back()?)
    }
}

impl ExactSizeIterator for FlatLeafIter<'_> {}

/// A branch page image, borrowed: separator keys in ascending order between
/// one more child than there are separators.
///
/// Child 0 is the first child. Child `i + 1` is the one stored with separator
/// `i`, and holds the keys that are greater than or equal to that separator
/// and less than the next one. No accessor allocates.
#[derive(Debug, Clone, Copy)]
pub struct FlatBranch<'a>(View<'a>);

impl<'a> FlatBranch<'a> {
    /// Bytes of the image before the prefix.
    pub const HEADER_LEN: usize = FlatPageKind::Branch.header_len();
    /// Bytes an entry takes on top of its separator suffix.
    pub const ENTRY_OVERHEAD: usize = FlatPageKind::Branch.entry_overhead();

    /// Bytes an entry adds to the image.
    pub const fn entry_len(separator_suffix_len: usize) -> usize {
        Self::ENTRY_OVERHEAD + separator_suffix_len
    }

    /// The image, at its exact length.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.0.image
    }

    /// The bytes every separator of the page starts with.
    pub const fn prefix(&self) -> &'a [u8] {
        self.0.prefix
    }

    /// The page's distance from the leaves, at least 1.
    pub const fn level(&self) -> u8 {
        self.0.level
    }

    /// The number of separators. There is one more child.
    pub const fn len(&self) -> usize {
        self.0.count
    }

    /// Whether the branch has no separator, and so a single child.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The separator at position `slot`, counting from 0 in key order.
    pub fn separator(&self, slot: usize) -> Option<FlatKey<'a>> {
        self.0.key(slot)
    }

    /// The child at position `index`, from 0 to [`Self::len`] inclusive.
    pub fn child(&self, index: usize) -> Option<PageId> {
        match index.checked_sub(1) {
            Some(slot) => {
                let (_, child) = self.0.split_cell(slot)?;
                page_id_at(child, 0)
            }
            None => page_id_at(self.0.image, FIRST_CHILD_AT),
        }
    }

    /// The position of the child whose range holds `key`: the number of
    /// separators less than or equal to `key`, found by binary search.
    pub fn child_index(&self, key: &[u8]) -> usize {
        match self.0.search(key) {
            Ok(slot) => slot + 1,
            Err(slot) => slot,
        }
    }

    /// The child whose range holds `key`.
    pub fn child_for(&self, key: &[u8]) -> Option<PageId> {
        self.child(self.child_index(key))
    }
}

#[derive(Debug, Clone, Copy)]
struct BuilderSlot {
    end: usize,
    key_len: usize,
    flags: u8,
}

#[derive(Debug, Clone)]
struct Builder {
    kind: FlatPageKind,
    capacity: usize,
    image: Vec<u8>,
    slots: Vec<BuilderSlot>,
    cells: Vec<u8>,
    last_suffix: Range<usize>,
}

impl Builder {
    fn new(
        kind: FlatPageKind,
        level: u8,
        first_child: Option<PageId>,
        prefix: &[u8],
        capacity: usize,
    ) -> Result<Self, FormatError> {
        if capacity > FlatPage::MAX_LEN {
            return Err(FlatPageError::CapacityOutOfRange { capacity }.into());
        }
        let needed = kind.header_len() + prefix.len();
        if needed > capacity {
            return Err(FlatPageError::PageFull { needed, capacity }.into());
        }
        let mut image = Vec::with_capacity(needed);
        image.extend_from_slice(&[kind.code(), level]);
        image.resize(PREFIX_LEN_AT, 0);
        image.extend_from_slice(&le16(prefix.len()));
        if let Some(first_child) = first_child {
            image.extend_from_slice(&first_child.encode());
        }
        image.extend_from_slice(prefix);
        Ok(Self {
            kind,
            capacity,
            image,
            slots: Vec::new(),
            cells: Vec::new(),
            last_suffix: 0..0,
        })
    }

    fn prefix(&self) -> &[u8] {
        self.image.get(self.kind.header_len()..).unwrap_or_default()
    }

    fn encoded_len(&self) -> usize {
        self.image.len() + self.slots.len() * self.kind.slot_len() + self.cells.len()
    }

    fn push(&mut self, key: &[u8], flags: u8, tail: &[u8]) -> Result<(), FormatError> {
        let suffix = key
            .strip_prefix(self.prefix())
            .ok_or(FlatPageError::KeyOutsidePrefix)?;
        let slot = self.slots.len();
        let last = self.cells.get(self.last_suffix.clone());
        if slot > 0 && last.is_none_or(|last| last >= suffix) {
            return Err(FlatPageError::KeysOutOfOrder { slot }.into());
        }
        let needed = self.encoded_len() + self.kind.slot_len() + suffix.len() + tail.len();
        if needed > self.capacity {
            return Err(FlatPageError::PageFull {
                needed,
                capacity: self.capacity,
            }
            .into());
        }
        let start = self.cells.len();
        self.cells.extend_from_slice(suffix);
        self.last_suffix = start..self.cells.len();
        self.cells.extend_from_slice(tail);
        self.slots.push(BuilderSlot {
            end: self.cells.len(),
            key_len: suffix.len(),
            flags,
        });
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        let len = self.encoded_len();
        let cells_at = len - self.cells.len();
        let mut image = self.image;
        image.reserve_exact(len - image.len());
        for slot in &self.slots {
            image.extend_from_slice(&le16(cells_at + slot.end));
            if self.kind == FlatPageKind::Leaf {
                image.extend_from_slice(&le16(slot.key_len));
                image.push(slot.flags);
            }
        }
        image.extend_from_slice(&self.cells);
        image[LEN_AT..CHECKSUM_AT].copy_from_slice(&le16(len));
        image[COUNT_AT..PREFIX_LEN_AT].copy_from_slice(&le16(self.slots.len()));
        let checksum = Checksum::of(&[&image[..CHECKSUM_AT], &image[COMMON_HEADER_LEN..]]);
        image[CHECKSUM_AT..COMMON_HEADER_LEN].copy_from_slice(&checksum.encode());
        image
    }
}

/// Builds a leaf page image in the current format version.
///
/// Entries are added in ascending key order. The image never grows past the
/// capacity the builder was created with.
#[derive(Debug, Clone)]
pub struct FlatLeafBuilder(Builder);

impl FlatLeafBuilder {
    /// An empty leaf whose keys all start with `prefix`, in an image of at
    /// most `capacity` bytes.
    pub fn new(prefix: &[u8], capacity: usize) -> Result<Self, FormatError> {
        Builder::new(FlatPageKind::Leaf, 0, None, prefix, capacity).map(Self)
    }

    /// Adds an entry. `key` is the whole key, prefix included, and must be
    /// greater than the key added before it.
    ///
    /// On any error the builder is unchanged. [`FlatPageError::PageFull`]
    /// means the entry belongs on another page.
    pub fn push(&mut self, key: &[u8], value: FlatValue<'_>) -> Result<(), FormatError> {
        self.0.push(key, value.flags(), value.bytes())
    }

    /// The number of entries added.
    pub fn len(&self) -> usize {
        self.0.slots.len()
    }

    /// Whether no entry has been added.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The length the image has if it is finished now.
    pub fn encoded_len(&self) -> usize {
        self.0.encoded_len()
    }

    /// The image.
    pub fn finish(self) -> Vec<u8> {
        self.0.finish()
    }
}

/// Builds a branch page image in the current format version.
///
/// Separators are added in ascending order, each with the child to its
/// right. The image never grows past the capacity the builder was created
/// with.
#[derive(Debug, Clone)]
pub struct FlatBranchBuilder(Builder);

impl FlatBranchBuilder {
    /// A branch `level` levels above the leaves with the single child
    /// `first_child`, whose separators all start with `prefix`, in an image
    /// of at most `capacity` bytes.
    pub fn new(
        prefix: &[u8],
        capacity: usize,
        level: u8,
        first_child: PageId,
    ) -> Result<Self, FormatError> {
        let kind = FlatPageKind::Branch;
        if level == 0 {
            return Err(FlatPageError::InvalidLevel { kind, level }.into());
        }
        Builder::new(kind, level, Some(first_child), prefix, capacity).map(Self)
    }

    /// Adds a separator and the child that holds the keys from it upward.
    /// `separator` is the whole key, prefix included, and must be greater
    /// than the separator added before it.
    ///
    /// On any error the builder is unchanged. [`FlatPageError::PageFull`]
    /// means the entry belongs on another page.
    pub fn push(&mut self, separator: &[u8], child: PageId) -> Result<(), FormatError> {
        self.0.push(separator, 0, &child.encode())
    }

    /// The number of separators added.
    pub fn len(&self) -> usize {
        self.0.slots.len()
    }

    /// Whether no separator has been added.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The length the image has if it is finished now.
    pub fn encoded_len(&self) -> usize {
        self.0.encoded_len()
    }

    /// The image.
    pub fn finish(self) -> Vec<u8> {
        self.0.finish()
    }
}

/// The number of leading bytes `a` and `b` share.
///
/// The longest prefix a page can use is what its smallest and largest key
/// share.
pub fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}

/// The shortest separator between two neighbouring keys: the shortest byte
/// string that is greater than `left` and not greater than `right`.
///
/// It is a prefix of `right`. `left` must be less than `right`; otherwise
/// the result is not a separator.
pub fn shortest_separator<'a>(left: &[u8], right: &'a [u8]) -> &'a [u8] {
    right
        .get(..=common_prefix_len(left, right))
        .unwrap_or(right)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::test_util::SplitMix64;

    const V1: FormatVersion = FormatVersion::V1;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Contents {
        Leaf {
            prefix: Vec<u8>,
            entries: Vec<(Vec<u8>, bool, Vec<u8>)>,
        },
        Branch {
            prefix: Vec<u8>,
            level: u8,
            first_child: PageId,
            entries: Vec<(Vec<u8>, PageId)>,
        },
    }

    impl Contents {
        fn encode(&self, capacity: usize) -> Result<Vec<u8>, FormatError> {
            match self {
                Self::Leaf { prefix, entries } => {
                    let mut builder = FlatLeafBuilder::new(prefix, capacity)?;
                    for (key, out_of_line, value) in entries {
                        builder.push(key, value_of(*out_of_line, value))?;
                    }
                    assert_eq!(builder.len(), entries.len());
                    assert_eq!(builder.is_empty(), entries.is_empty());
                    assert_eq!(builder.encoded_len(), self.encoded_len());
                    Ok(builder.finish())
                }
                Self::Branch {
                    prefix,
                    level,
                    first_child,
                    entries,
                } => {
                    let mut builder =
                        FlatBranchBuilder::new(prefix, capacity, *level, *first_child)?;
                    for (separator, child) in entries {
                        builder.push(separator, *child)?;
                    }
                    assert_eq!(builder.len(), entries.len());
                    assert_eq!(builder.is_empty(), entries.is_empty());
                    assert_eq!(builder.encoded_len(), self.encoded_len());
                    Ok(builder.finish())
                }
            }
        }

        fn image(&self) -> Vec<u8> {
            self.encode(FlatPage::MAX_LEN).expect("fits")
        }

        fn encoded_len(&self) -> usize {
            match self {
                Self::Leaf { prefix, entries } => {
                    12 + prefix.len()
                        + entries
                            .iter()
                            .map(|(key, _, value)| 5 + key.len() - prefix.len() + value.len())
                            .sum::<usize>()
                }
                Self::Branch {
                    prefix, entries, ..
                } => {
                    16 + prefix.len()
                        + entries
                            .iter()
                            .map(|(key, _)| 6 + key.len() - prefix.len())
                            .sum::<usize>()
                }
            }
        }

        fn read(page: FlatPage<'_>) -> Self {
            match page {
                FlatPage::Leaf(leaf) => Self::Leaf {
                    prefix: leaf.prefix().to_vec(),
                    entries: (0..leaf.len())
                        .map(|slot| owned_entry(leaf.entry(slot).expect("in range")))
                        .collect(),
                },
                FlatPage::Branch(branch) => Self::Branch {
                    prefix: branch.prefix().to_vec(),
                    level: branch.level(),
                    first_child: branch.child(0).expect("first child"),
                    entries: (0..branch.len())
                        .map(|slot| {
                            (
                                branch.separator(slot).expect("in range").to_vec(),
                                branch.child(slot + 1).expect("in range"),
                            )
                        })
                        .collect(),
                },
            }
        }

        fn keys(&self) -> Vec<&[u8]> {
            match self {
                Self::Leaf { entries, .. } => entries.iter().map(|e| e.0.as_slice()).collect(),
                Self::Branch { entries, .. } => entries.iter().map(|e| e.0.as_slice()).collect(),
            }
        }

        fn prefix(&self) -> &[u8] {
            match self {
                Self::Leaf { prefix, .. } | Self::Branch { prefix, .. } => prefix,
            }
        }
    }

    fn value_of(out_of_line: bool, bytes: &[u8]) -> FlatValue<'_> {
        if out_of_line {
            FlatValue::OutOfLine(bytes)
        } else {
            FlatValue::Inline(bytes)
        }
    }

    fn owned_entry((key, value): (FlatKey<'_>, FlatValue<'_>)) -> (Vec<u8>, bool, Vec<u8>) {
        (key.to_vec(), !value.is_inline(), value.bytes().to_vec())
    }

    fn decode_leaf(image: &[u8]) -> FlatLeaf<'_> {
        FlatPage::decode(image, V1)
            .expect("valid")
            .leaf()
            .expect("leaf")
    }

    fn decode_branch(image: &[u8]) -> FlatBranch<'_> {
        FlatPage::decode(image, V1)
            .expect("valid")
            .branch()
            .expect("branch")
    }

    fn assert_encodes_back(page: FlatPage<'_>) {
        assert_eq!(Contents::read(page).image(), page.as_bytes());
    }

    fn page_id(raw: u32) -> PageId {
        PageId::new(raw).expect("non-zero")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn below(rng: &mut SplitMix64, bound: usize) -> usize {
        (rng.next_u64() % bound as u64) as usize
    }

    fn random_keys(rng: &mut SplitMix64, prefix: &[u8], count: usize) -> Vec<Vec<u8>> {
        const ALPHABET: [u8; 5] = [0x00, 0x01, 0x7F, 0x80, 0xFF];
        let mut keys = BTreeSet::new();
        while keys.len() < count {
            let suffix: Vec<u8> = if below(rng, 8) == 0 {
                let len = below(rng, 40);
                rng.bytes(len)
            } else {
                (0..below(rng, 7))
                    .map(|_| ALPHABET[below(rng, ALPHABET.len())])
                    .collect()
            };
            keys.insert([prefix, &suffix].concat());
        }
        keys.into_iter().collect()
    }

    fn random_contents(rng: &mut SplitMix64, leaf: bool) -> Contents {
        let prefix_len = [0, 0, 1, 5, 32, 40][below(rng, 6)];
        let prefix = rng.bytes(prefix_len);
        let count = [0, 1, 2, 3, 17, 60][below(rng, 6)];
        let keys = random_keys(rng, &prefix, count);
        if leaf {
            let entries = keys
                .into_iter()
                .map(|key| {
                    let len = below(rng, 40);
                    (key, below(rng, 4) == 0, rng.bytes(len))
                })
                .collect();
            Contents::Leaf { prefix, entries }
        } else {
            let entries = keys
                .into_iter()
                .map(|key| (key, page_id(rng.next_u64() as u32 | 1)))
                .collect();
            Contents::Branch {
                prefix,
                level: 1 + below(rng, 255) as u8,
                first_child: page_id(rng.next_u64() as u32 | 1),
                entries,
            }
        }
    }

    fn random_pages(seed: u64, count: usize) -> Vec<Contents> {
        let mut rng = SplitMix64::new(seed);
        (0..count)
            .map(|index| random_contents(&mut rng, index % 2 == 0))
            .collect()
    }

    fn probes(rng: &mut SplitMix64, contents: &Contents) -> Vec<Vec<u8>> {
        let prefix = contents.prefix();
        let mut probes = vec![Vec::new(), prefix.to_vec(), vec![0xFF; 48]];
        for cut in 0..prefix.len() {
            probes.push(prefix[..cut].to_vec());
            let mut bumped = prefix[..=cut].to_vec();
            bumped[cut] = bumped[cut].wrapping_add(1);
            probes.push(bumped.clone());
            bumped[cut] = bumped[cut].wrapping_sub(2);
            probes.push(bumped);
        }
        for key in contents.keys() {
            probes.push(key.to_vec());
            probes.push([key, &[0]].concat());
            if let Some((&last, head)) = key.split_last() {
                probes.push(head.to_vec());
                probes.push([head, &[last.wrapping_add(1)]].concat());
                probes.push([head, &[last.wrapping_sub(1)]].concat());
            }
        }
        probes.extend(random_keys(rng, prefix, 20));
        probes.extend((0..20).map(|_| {
            let len = below(rng, 50);
            rng.bytes(len)
        }));
        probes
    }

    fn seal(mut image: Vec<u8>) -> Vec<u8> {
        let len = image.len();
        image[LEN_AT..CHECKSUM_AT].copy_from_slice(&(len as u16).to_le_bytes());
        reseal(&mut image);
        image
    }

    fn reseal(bytes: &mut [u8]) {
        let len = usize::from(u16::from_le_bytes([bytes[2], bytes[3]])).min(bytes.len());
        if len >= COMMON_HEADER_LEN {
            let checksum = Checksum::of(&[&bytes[..CHECKSUM_AT], &bytes[COMMON_HEADER_LEN..len]]);
            bytes[CHECKSUM_AT..COMMON_HEADER_LEN].copy_from_slice(&checksum.encode());
        }
    }

    fn raw(kind: u8, level: u8, count: u16, prefix_len: u16, rest: &[u8]) -> Vec<u8> {
        let mut image = vec![kind, level, 0, 0, 0, 0, 0, 0];
        image.extend_from_slice(&count.to_le_bytes());
        image.extend_from_slice(&prefix_len.to_le_bytes());
        image.extend_from_slice(rest);
        seal(image)
    }

    fn raw_leaf(prefix: &[u8], slots: &[(u16, u16, u8)], cells: &[u8]) -> Vec<u8> {
        let mut rest = prefix.to_vec();
        for &(end, key_len, flags) in slots {
            rest.extend_from_slice(&end.to_le_bytes());
            rest.extend_from_slice(&key_len.to_le_bytes());
            rest.push(flags);
        }
        rest.extend_from_slice(cells);
        raw(1, 0, slots.len() as u16, prefix.len() as u16, &rest)
    }

    fn raw_branch(first_child: u32, ends: &[u16], cells: &[u8]) -> Vec<u8> {
        let mut rest = first_child.to_le_bytes().to_vec();
        for end in ends {
            rest.extend_from_slice(&end.to_le_bytes());
        }
        rest.extend_from_slice(cells);
        raw(2, 1, ends.len() as u16, 0, &rest)
    }

    fn decode_error(bytes: &[u8]) -> FormatError {
        let error = FlatPage::decode(bytes, V1).expect_err("not a flat page");
        exercise(bytes);
        error
    }

    fn flat(error: FlatPageError) -> FormatError {
        FormatError::FlatPage(error)
    }

    fn exercise(bytes: &[u8]) {
        let pages = [
            FlatPage::decode(bytes, V1),
            FlatPage::from_validated(bytes, V1),
        ];
        for page in pages.into_iter().flatten() {
            let probes: [&[u8]; 4] = [b"", page.prefix(), &[0xFF; 40], bytes];
            let positions = (0..page.len() + 2).chain([usize::MAX / 5, usize::MAX]);
            assert!(page.as_bytes().len() <= bytes.len());
            match page {
                FlatPage::Leaf(leaf) => {
                    for slot in positions {
                        let _ = leaf.key(slot).map(|key| (key.to_vec(), key.is_empty()));
                        let _ = leaf.value(slot).map(|value| value.bytes().len());
                    }
                    assert!(leaf.iter().count() <= leaf.len());
                    assert!(leaf.iter().rev().count() <= leaf.len());
                    for probe in probes {
                        let slot = leaf.search(probe).unwrap_or_else(|slot| slot);
                        assert!(slot <= leaf.len());
                        let _ = leaf.get(probe);
                    }
                }
                FlatPage::Branch(branch) => {
                    for index in positions {
                        let _ = branch.child(index);
                        let _ = branch.separator(index).map(|key| key.to_vec());
                    }
                    for probe in probes {
                        assert!(branch.child_index(probe) <= branch.len());
                        let _ = branch.child_for(probe);
                    }
                }
            }
        }
    }

    #[test]
    fn kinds_and_sizes_are_part_of_the_format() {
        assert_eq!(FlatPageKind::ALL.map(FlatPageKind::code), [1, 2]);
        assert_eq!(FlatPageKind::ALL.map(FlatPageKind::header_len), [12, 16]);
        assert_eq!(FlatPageKind::ALL.map(FlatPageKind::entry_overhead), [5, 6]);
        assert_eq!(FlatPage::MAX_LEN, 65_535);
        assert_eq!(FlatLeaf::HEADER_LEN, 12);
        assert_eq!(FlatLeaf::entry_len(3, 4), 12);
        assert_eq!(FlatBranch::HEADER_LEN, 16);
        assert_eq!(FlatBranch::entry_len(3), 9);
        let mut known = 0;
        for code in 0..=u8::MAX {
            match FlatPageKind::from_code(code, V1) {
                Ok(kind) => {
                    assert_eq!(kind.code(), code);
                    known += 1;
                }
                Err(error) => assert_eq!(
                    error,
                    FormatError::UnknownRecordKind {
                        code: u32::from(code)
                    }
                ),
            }
        }
        assert_eq!(known, FlatPageKind::ALL.len());
    }

    #[test]
    fn pages_match_the_golden_bytes() {
        let leaf = Contents::Leaf {
            prefix: b"user/".to_vec(),
            entries: vec![
                (b"user/alice".to_vec(), false, b"A1".to_vec()),
                (b"user/bob".to_vec(), true, vec![1, 2, 3, 4]),
                (b"user/carol".to_vec(), false, Vec::new()),
            ],
        };
        let branch = Contents::Branch {
            prefix: b"user/".to_vec(),
            level: 1,
            first_child: page_id(7),
            entries: vec![
                (b"user/b".to_vec(), page_id(9)),
                (b"user/carol".to_vec(), page_id(12)),
            ],
        };
        let empty_leaf = Contents::Leaf {
            prefix: Vec::new(),
            entries: Vec::new(),
        };
        let single_child = Contents::Branch {
            prefix: Vec::new(),
            level: 255,
            first_child: PageId::MAX,
            entries: Vec::new(),
        };
        let golden = [
            (
                leaf,
                "010033004df1d0f803000500757365722f27000500002e000300013300050000\
                 616c6963654131626f62010203046361726f6c",
            ),
            (
                branch,
                "020127009dd0b9920200050007000000757365722f1e00270062090000006361\
                 726f6c0c000000",
            ),
            (empty_leaf, "01000c00a71a0d6400000000"),
            (single_child, "02ff10003e7a0dac00000000ffffffff"),
        ];
        for (contents, image) in golden {
            let encoded = contents.image();
            assert_eq!(hex(&encoded), image);
            assert_eq!(
                Contents::read(FlatPage::decode(&encoded, V1).expect("valid")),
                contents
            );
        }
    }

    #[test]
    fn random_pages_round_trip_in_both_directions() {
        let mut rng = SplitMix64::new(0xF1A7);
        for contents in random_pages(0x04, 2000) {
            let image = contents.image();
            assert_eq!(image.len(), contents.encoded_len());

            let page = FlatPage::decode(&image, V1).expect("valid");
            assert_eq!(page.as_bytes(), image);
            assert_eq!(page.prefix(), contents.prefix());
            assert_eq!(page.len(), contents.keys().len());
            assert_eq!(page.is_empty(), contents.keys().is_empty());
            assert_eq!(page.leaf().is_some(), page.kind() == FlatPageKind::Leaf);
            assert_eq!(page.branch().is_some(), page.kind() == FlatPageKind::Branch);
            assert_eq!(page.level() == 0, page.kind() == FlatPageKind::Leaf);
            assert_eq!(Contents::read(page), contents);
            assert_eq!(
                Contents::read(page).encode(image.len()).expect("fits"),
                image
            );

            let trusted = FlatPage::from_validated(&image, V1).expect("valid");
            assert_eq!(Contents::read(trusted), contents);

            let mut frame = image.clone();
            let slack = 1 + below(&mut rng, 64);
            frame.extend(rng.bytes(slack));
            let framed = FlatPage::decode(&frame, V1).expect("valid");
            assert_eq!(framed.as_bytes(), image);
            exercise(&frame);
        }
    }

    #[test]
    fn leaf_search_agrees_with_a_sorted_model() {
        let mut rng = SplitMix64::new(0x5EA2C4);
        for contents in random_pages(0x1EAF, 600).into_iter().step_by(2) {
            let Contents::Leaf { entries, .. } = &contents else {
                panic!("even pages are leaves");
            };
            let model: BTreeMap<&[u8], (bool, &[u8])> = entries
                .iter()
                .map(|(key, out_of_line, value)| (key.as_slice(), (*out_of_line, value.as_slice())))
                .collect();
            let keys = contents.keys();
            let image = contents.image();
            let leaf = decode_leaf(&image);
            for probe in probes(&mut rng, &contents) {
                assert_eq!(
                    leaf.search(&probe),
                    keys.binary_search(&probe.as_slice()),
                    "probe {probe:?} in {keys:?}"
                );
                assert_eq!(
                    leaf.get(&probe),
                    model
                        .get(probe.as_slice())
                        .map(|&(out_of_line, value)| value_of(out_of_line, value))
                );
            }
        }
    }

    #[test]
    fn branch_routes_each_key_to_the_child_a_model_picks() {
        let mut rng = SplitMix64::new(0xB2A4C4);
        for contents in random_pages(0xB2A, 600).into_iter().skip(1).step_by(2) {
            let Contents::Branch {
                first_child,
                entries,
                ..
            } = &contents
            else {
                panic!("odd pages are branches");
            };
            let image = contents.image();
            let branch = decode_branch(&image);
            assert_eq!(branch.child(entries.len() + 1), None);
            assert_eq!(branch.separator(entries.len()), None);
            for probe in probes(&mut rng, &contents) {
                let index = entries.partition_point(|(separator, _)| separator <= &probe);
                let child = match index.checked_sub(1) {
                    Some(slot) => entries[slot].1,
                    None => *first_child,
                };
                assert_eq!(branch.child_index(&probe), index, "probe {probe:?}");
                assert_eq!(branch.child_for(&probe), Some(child));
            }
        }
    }

    #[test]
    fn leaf_iteration_runs_both_ways_and_knows_its_length() {
        for contents in random_pages(0x17E2, 200).into_iter().step_by(2) {
            let Contents::Leaf { entries, .. } = &contents else {
                panic!("even pages are leaves");
            };
            let image = contents.image();
            let leaf = decode_leaf(&image);
            assert_eq!(leaf.iter().len(), entries.len());
            assert_eq!(leaf.is_empty(), entries.is_empty());
            assert_eq!(&leaf.iter().map(owned_entry).collect::<Vec<_>>(), entries);
            let mut backward: Vec<_> = leaf.into_iter().rev().map(owned_entry).collect();
            backward.reverse();
            assert_eq!(&backward, entries);
            assert_eq!(leaf.entry(entries.len()), None);
            assert!(leaf.iter().is_sorted_by(|a, b| a.0 < b.0));
        }
    }

    #[test]
    fn keys_compare_by_their_bytes_wherever_the_prefix_ends() {
        let mut rng = SplitMix64::new(0xC0A7);
        let keys = random_keys(&mut rng, b"shared/", 40);
        let page = |prefix_len: usize| {
            let mut builder = FlatLeafBuilder::new(&b"shared/"[..prefix_len], 4096).expect("fits");
            for key in &keys {
                builder.push(key, FlatValue::Inline(b"")).expect("fits");
            }
            builder.finish()
        };
        let (long, short) = (page(7), page(2));
        let long = decode_leaf(&long);
        let short = decode_leaf(&short);
        for (a, key_a) in keys.iter().enumerate() {
            let in_long = long.key(a).expect("in range");
            assert_eq!(in_long.prefix(), b"shared/");
            assert_eq!([in_long.prefix(), in_long.suffix()].concat(), *key_a);
            assert_eq!(in_long.len(), key_a.len());
            assert!(in_long == **key_a);
            let mut appended = vec![0xAA];
            in_long.append_to(&mut appended);
            assert_eq!(appended[1..], **key_a);
            for (b, key_b) in keys.iter().enumerate() {
                let in_short = short.key(b).expect("in range");
                assert_eq!(in_long.cmp(&in_short), key_a.cmp(key_b));
                assert_eq!(in_long == in_short, a == b);
                assert_eq!(in_long.cmp_bytes(key_b), key_a.cmp(key_b));
                assert_eq!(in_short.cmp_bytes(key_a), key_b.cmp(key_a));
            }
            for cut in 0..key_a.len() {
                assert_eq!(in_long.cmp_bytes(&key_a[..cut]), Ordering::Greater);
            }
        }
    }

    #[test]
    fn every_shorter_input_is_truncated() {
        for contents in random_pages(0x7214, 40) {
            let image = contents.image();
            for available in 0..image.len() {
                let needed = if available < 8 { 8 } else { image.len() };
                let truncated = FormatError::Truncated { needed, available };
                assert_eq!(decode_error(&image[..available]), truncated);
                assert_eq!(
                    FlatPage::from_validated(&image[..available], V1).expect_err("truncated"),
                    truncated
                );
            }
        }
    }

    #[test]
    fn every_single_bit_flip_is_rejected() {
        for contents in random_pages(0xF11B, 40) {
            let image = contents.image();
            for bit in 0..image.len() * 8 {
                let mut damaged = image.clone();
                damaged[bit / 8] ^= 1 << (bit % 8);
                let error = decode_error(&damaged);
                if (LEN_AT..CHECKSUM_AT).contains(&(bit / 8)) {
                    assert!(matches!(
                        error,
                        FormatError::Truncated { .. }
                            | FormatError::ChecksumMismatch { .. }
                            | FormatError::FlatPage(FlatPageError::ImageTooShort { .. })
                    ));
                } else {
                    assert!(matches!(error, FormatError::ChecksumMismatch { .. }));
                }
            }
        }
    }

    #[test]
    fn damage_under_a_valid_checksum_decodes_only_to_what_encodes_back() {
        let mut rng = SplitMix64::new(0xDA3A6E);
        let (mut accepted, mut rejected) = (0, 0);
        for contents in random_pages(0x5EA1, 400) {
            let image = contents.image();
            for _ in 0..200 {
                let mut damaged = image.clone();
                for _ in 0..1 + below(&mut rng, 3) {
                    let at = below(&mut rng, damaged.len());
                    damaged[at] = match below(&mut rng, 4) {
                        0 => rng.next_u64() as u8,
                        1 => damaged[at].wrapping_add(1),
                        2 => damaged[at].wrapping_sub(1),
                        _ => damaged[at] ^ (1 << below(&mut rng, 8)),
                    };
                }
                reseal(&mut damaged);
                exercise(&damaged);
                match FlatPage::decode(&damaged, V1) {
                    Ok(page) => {
                        assert_encodes_back(page);
                        accepted += 1;
                    }
                    Err(_) => rejected += 1,
                }
            }
        }
        assert!(accepted > 1000, "accepted {accepted}");
        assert!(rejected > 1000, "rejected {rejected}");
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut rng = SplitMix64::new(0xA2B1);
        let (mut opened, mut decoded) = (0, 0);
        for round in 0..200_000 {
            let len = below(&mut rng, 120);
            let mut bytes = rng.bytes(len);
            if round % 4 != 0 && len >= 12 {
                bytes[0] = 1 + (round % 2) as u8;
                bytes[1] = if bytes[0] == 1 { 0 } else { bytes[1] | 1 };
                let image_len = 12 + below(&mut rng, len - 11);
                bytes[2..4].copy_from_slice(&(image_len as u16).to_le_bytes());
                if round % 3 == 0 {
                    bytes[8..12].copy_from_slice(&[(round % 5) as u8, 0, (round % 7) as u8, 0]);
                }
                reseal(&mut bytes);
            }
            exercise(&bytes);
            opened += usize::from(FlatPage::from_validated(&bytes, V1).is_ok());
            if let Ok(page) = FlatPage::decode(&bytes, V1) {
                assert_encodes_back(page);
                decoded += 1;
            }
        }
        assert!(opened > 10_000, "opened {opened}");
        assert!(decoded > 100, "decoded {decoded}");
    }

    #[test]
    fn zero_filled_bytes_are_not_a_page() {
        for len in 8..64 {
            assert_eq!(
                decode_error(&vec![0; len]),
                flat(FlatPageError::ImageTooShort {
                    len: 0,
                    header_len: 12
                })
            );
        }
    }

    #[test]
    fn the_header_is_checked_in_order() {
        let mut short = raw(1, 0, 0, 0, &[]);
        short[2] = 11;
        assert_eq!(
            decode_error(&short),
            flat(FlatPageError::ImageTooShort {
                len: 11,
                header_len: 12
            })
        );

        let mut unsealed = raw(9, 7, 1, 0, &[]);
        unsealed[4] ^= 1;
        assert!(matches!(
            decode_error(&unsealed),
            FormatError::ChecksumMismatch { .. }
        ));

        for code in (0..=u8::MAX).filter(|code| !(1..=2).contains(code)) {
            assert_eq!(
                decode_error(&raw(code, 7, 1, 0, &[])),
                FormatError::UnknownRecordKind {
                    code: u32::from(code)
                }
            );
        }

        for level in 1..=u8::MAX {
            assert_eq!(
                decode_error(&raw(1, level, 9, 9, &[])),
                flat(FlatPageError::InvalidLevel {
                    kind: FlatPageKind::Leaf,
                    level
                })
            );
        }
        assert_eq!(
            decode_error(&raw(2, 0, 9, 9, &[0; 4])),
            flat(FlatPageError::InvalidLevel {
                kind: FlatPageKind::Branch,
                level: 0
            })
        );

        for extra in 0..4 {
            assert_eq!(
                decode_error(&raw(2, 1, 0, 0, &[1, 0, 0, 0][..extra])),
                flat(FlatPageError::ImageTooShort {
                    len: 12 + extra,
                    header_len: 16
                })
            );
        }

        assert_eq!(
            decode_error(&raw(2, 1, 9, 9, &[0; 4])),
            FormatError::MissingId
        );

        assert_eq!(
            decode_error(&raw(1, 0, 1, 0, &[])),
            flat(FlatPageError::DirectoryOutOfBounds {
                needed: 17,
                len: 12
            })
        );
        assert_eq!(
            decode_error(&raw(1, 0, 0, 3, b"ab")),
            flat(FlatPageError::DirectoryOutOfBounds {
                needed: 15,
                len: 14
            })
        );
        assert_eq!(
            decode_error(&raw(2, 1, 2, 1, &[7, 0, 0, 0, b'p', 0, 0, 0])),
            flat(FlatPageError::DirectoryOutOfBounds {
                needed: 21,
                len: 20
            })
        );
        assert_eq!(
            decode_error(&raw(1, 0, u16::MAX, u16::MAX, &[0; 40])),
            flat(FlatPageError::DirectoryOutOfBounds {
                needed: 12 + 65_535 + 65_535 * 5,
                len: 52
            })
        );
    }

    #[test]
    fn each_leaf_slot_must_describe_its_cell() {
        let with_slots = |slots: &[(u16, u16, u8)]| raw_leaf(b"p", slots, b"aaXbYYY");
        let valid = with_slots(&[(26, 2, 0), (30, 1, 1)]);
        let leaf = decode_leaf(&valid);
        assert_eq!(leaf.get(b"paa"), Some(FlatValue::Inline(b"X")));
        assert_eq!(leaf.get(b"pb"), Some(FlatValue::OutOfLine(b"YYY")));

        let invalid = |slot| flat(FlatPageError::InvalidSlot { slot });
        let cases = [
            (with_slots(&[(22, 2, 0), (30, 1, 0)]), invalid(0)),
            (with_slots(&[(31, 2, 0), (30, 1, 0)]), invalid(0)),
            (with_slots(&[(26, 4, 0), (30, 1, 0)]), invalid(0)),
            (with_slots(&[(26, 2, 0), (25, 1, 0)]), invalid(1)),
            (with_slots(&[(26, 2, 0), (31, 1, 0)]), invalid(1)),
            (with_slots(&[(26, 2, 0), (30, 5, 0)]), invalid(1)),
            (
                with_slots(&[(26, 2, 2), (30, 1, 0)]),
                FormatError::NonZeroReserved,
            ),
            (
                with_slots(&[(26, 2, 0), (30, 1, 0x81)]),
                FormatError::NonZeroReserved,
            ),
            (
                raw_leaf(b"p", &[(26, 2, 0), (30, 2, 0)], b"aaXaaYY"),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 }),
            ),
            (
                raw_leaf(b"p", &[(26, 2, 0), (30, 1, 0)], b"aaXaYYY"),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 }),
            ),
            (
                raw_leaf(b"p", &[(23, 0, 0), (23, 0, 0)], b""),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 }),
            ),
            (
                with_slots(&[(26, 2, 0), (29, 1, 0)]),
                flat(FlatPageError::UnusedBytes { used: 29, len: 30 }),
            ),
            (
                raw_leaf(b"p", &[], b"x"),
                flat(FlatPageError::UnusedBytes { used: 13, len: 14 }),
            ),
            (with_slots(&[(26, 4, 2), (20, 1, 0)]), invalid(0)),
            (
                with_slots(&[(26, 2, 2), (20, 1, 0)]),
                FormatError::NonZeroReserved,
            ),
        ];
        for (image, error) in cases {
            assert_eq!(decode_error(&image), error, "{}", hex(&image));
        }
    }

    #[test]
    fn each_branch_slot_must_describe_its_cell() {
        let cells = [b"a".as_slice(), &[9, 0, 0, 0], b"bb", &[12, 0, 0, 0]].concat();
        let valid = raw_branch(7, &[25, 31], &cells);
        let branch = decode_branch(&valid);
        assert_eq!(branch.child_for(b""), Some(page_id(7)));
        assert_eq!(branch.child_for(b"a"), Some(page_id(9)));
        assert_eq!(branch.child_for(b"bb"), Some(page_id(12)));

        let invalid = |slot| flat(FlatPageError::InvalidSlot { slot });
        let no_child = [b"a".as_slice(), &[0; 4], b"bb", &[12, 0, 0, 0]].concat();
        let same = [b"a".as_slice(), &[9, 0, 0, 0], b"a", &[12, 0, 0, 0], b"x"].concat();
        let cases = [
            (raw_branch(7, &[19, 31], &cells), invalid(0)),
            (raw_branch(7, &[23, 31], &cells), invalid(0)),
            (raw_branch(7, &[25, 28], &cells), invalid(1)),
            (raw_branch(7, &[25, 32], &cells), invalid(1)),
            (raw_branch(7, &[25, 31], &no_child), FormatError::MissingId),
            (
                raw_branch(7, &[25, 30], &same),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 }),
            ),
            (
                raw_branch(7, &[24, 28], &[[9, 0, 0, 0], [12, 0, 0, 0]].concat()),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 }),
            ),
            (
                raw_branch(7, &[25, 30], &cells),
                flat(FlatPageError::UnusedBytes { used: 30, len: 31 }),
            ),
            (
                raw_branch(7, &[], b"x"),
                flat(FlatPageError::UnusedBytes { used: 16, len: 17 }),
            ),
        ];
        for (image, error) in cases {
            assert_eq!(decode_error(&image), error, "{}", hex(&image));
        }
    }

    #[test]
    fn a_builder_rejects_what_is_not_a_page() {
        assert_eq!(
            FlatLeafBuilder::new(b"", 65_536).expect_err("too large"),
            flat(FlatPageError::CapacityOutOfRange { capacity: 65_536 })
        );
        assert_eq!(
            FlatBranchBuilder::new(b"", usize::MAX, 1, page_id(1)).expect_err("too large"),
            flat(FlatPageError::CapacityOutOfRange {
                capacity: usize::MAX
            })
        );
        assert_eq!(
            FlatLeafBuilder::new(b"", 11).expect_err("no room"),
            flat(FlatPageError::PageFull {
                needed: 12,
                capacity: 11
            })
        );
        assert_eq!(
            FlatBranchBuilder::new(b"abc", 18, 1, page_id(1)).expect_err("no room"),
            flat(FlatPageError::PageFull {
                needed: 19,
                capacity: 18
            })
        );
        assert_eq!(
            FlatBranchBuilder::new(b"", 4096, 0, page_id(1)).expect_err("level"),
            flat(FlatPageError::InvalidLevel {
                kind: FlatPageKind::Branch,
                level: 0
            })
        );

        let mut leaf = FlatLeafBuilder::new(b"ab", 4096).expect("fits");
        let value = FlatValue::Inline(b"v");
        for outside in [b"".as_slice(), b"a", b"ac", b"b", b"aab"] {
            assert_eq!(
                leaf.push(outside, value).expect_err("outside"),
                flat(FlatPageError::KeyOutsidePrefix)
            );
        }
        leaf.push(b"abm", value).expect("fits");
        for not_greater in [b"ab".as_slice(), b"abl", b"abm"] {
            assert_eq!(
                leaf.push(not_greater, value).expect_err("order"),
                flat(FlatPageError::KeysOutOfOrder { slot: 1 })
            );
        }
        leaf.push(b"abm\0", value).expect("fits");
        assert_eq!(leaf.len(), 2);

        let mut branch = FlatBranchBuilder::new(b"", 4096, 3, page_id(1)).expect("fits");
        branch.push(b"", page_id(2)).expect("fits");
        assert_eq!(
            branch.push(b"", page_id(3)).expect_err("order"),
            flat(FlatPageError::KeysOutOfOrder { slot: 1 })
        );
        assert_eq!(branch.len(), 1);
    }

    #[test]
    fn a_full_page_refuses_an_entry_and_stays_as_it_was() {
        for capacity in [12, 13, 64, 512, 4096, 16_384, FlatPage::MAX_LEN] {
            let mut rng = SplitMix64::new(capacity as u64);
            let mut leaf = FlatLeafBuilder::new(b"", capacity).expect("fits");
            let mut refused = 0;
            for index in 0u32..20_000 {
                let key = index.to_be_bytes();
                let len = below(&mut rng, 300);
                let value = rng.bytes(len);
                let before = leaf.clone().finish();
                let needed = before.len() + FlatLeaf::entry_len(key.len(), value.len());
                match leaf.push(&key, FlatValue::Inline(&value)) {
                    Ok(()) => assert_eq!(leaf.encoded_len(), needed),
                    Err(error) => {
                        assert!(needed > capacity);
                        assert_eq!(error, flat(FlatPageError::PageFull { needed, capacity }));
                        assert_eq!(leaf.clone().finish(), before);
                        refused += 1;
                    }
                }
            }
            assert!(refused > 0);
            let image = leaf.finish();
            assert!(image.len() <= capacity);
            assert!(FlatPage::decode(&image, V1).is_ok());
        }

        let mut branch = FlatBranchBuilder::new(b"k", 31, 1, page_id(1)).expect("fits");
        branch.push(b"k1", page_id(2)).expect("fits");
        let before = branch.clone().finish();
        assert_eq!(
            branch.push(b"k22", page_id(3)).expect_err("full"),
            flat(FlatPageError::PageFull {
                needed: 32,
                capacity: 31
            })
        );
        assert_eq!(branch.clone().finish(), before);
        branch.push(b"k2", page_id(3)).expect("fits");
        assert_eq!(branch.encoded_len(), 31);
        assert!(FlatPage::decode(&branch.finish(), V1).is_ok());
    }

    #[test]
    fn the_largest_image_is_65535_bytes() {
        let value =
            vec![0x5A; FlatPage::MAX_LEN - FlatLeaf::HEADER_LEN - FlatLeaf::entry_len(1, 0)];
        let mut leaf = FlatLeafBuilder::new(b"", FlatPage::MAX_LEN).expect("fits");
        assert_eq!(
            leaf.push(b"kk", FlatValue::Inline(&value))
                .expect_err("full"),
            flat(FlatPageError::PageFull {
                needed: 65_536,
                capacity: 65_535
            })
        );
        leaf.push(b"k", FlatValue::Inline(&value)).expect("fits");
        let image = leaf.finish();
        assert_eq!(image.len(), FlatPage::MAX_LEN);
        let page = FlatPage::decode(&image, V1).expect("valid");
        assert_eq!(
            page.leaf().expect("leaf").get(b"k"),
            Some(FlatValue::Inline(&value))
        );

        let prefix = vec![0xEE; FlatPage::MAX_LEN - FlatBranch::HEADER_LEN];
        let branch = FlatBranchBuilder::new(&prefix, FlatPage::MAX_LEN, 1, page_id(1))
            .expect("fits")
            .finish();
        assert_eq!(branch.len(), FlatPage::MAX_LEN);
        let page = FlatPage::decode(&branch, V1).expect("valid");
        assert_eq!(page.prefix(), prefix);
        assert_eq!(
            page.branch().expect("branch").child_for(&prefix),
            Some(page_id(1))
        );

        let mut many = FlatLeafBuilder::new(b"", FlatPage::MAX_LEN).expect("fits");
        let mut count = 0u16;
        while many
            .push(&count.to_be_bytes(), FlatValue::Inline(b""))
            .is_ok()
        {
            count += 1;
        }
        assert_eq!(usize::from(count), (FlatPage::MAX_LEN - 12) / 7);
        let image = many.finish();
        let leaf = decode_leaf(&image);
        assert_eq!(leaf.len(), usize::from(count));
        assert_eq!(leaf.search(&(count - 1).to_be_bytes()), Ok(leaf.len() - 1));
    }

    #[test]
    fn prefix_and_separator_helpers() {
        assert_eq!(common_prefix_len(b"", b""), 0);
        assert_eq!(common_prefix_len(b"abc", b"abd"), 2);
        assert_eq!(common_prefix_len(b"abc", b"abcd"), 3);
        assert_eq!(common_prefix_len(b"abc", b"xbc"), 0);
        assert_eq!(shortest_separator(b"apple", b"banana"), b"b");
        assert_eq!(shortest_separator(b"car", b"cart"), b"cart");
        assert_eq!(shortest_separator(b"", b"\0"), b"\0");
        assert_eq!(shortest_separator(b"b", b"a"), b"a");
        assert_eq!(shortest_separator(b"same", b"same"), b"same");

        let mut rng = SplitMix64::new(0x5E9A);
        let keys = random_keys(&mut rng, b"", 400);
        for pair in keys.windows(2) {
            let (left, right) = (pair[0].as_slice(), pair[1].as_slice());
            let separator = shortest_separator(left, right);
            assert!(right.starts_with(separator));
            assert!(left < separator && separator <= right);
            assert!(right[..separator.len() - 1] <= *left);
            assert_eq!(
                common_prefix_len(left, right),
                separator.len() - 1,
                "{left:?} {right:?}"
            );
        }
    }

    #[test]
    fn errors_say_what_is_wrong() {
        let errors = [
            FlatPageError::ImageTooShort {
                len: 3,
                header_len: 12,
            },
            FlatPageError::InvalidLevel {
                kind: FlatPageKind::Leaf,
                level: 4,
            },
            FlatPageError::DirectoryOutOfBounds {
                needed: 40,
                len: 30,
            },
            FlatPageError::InvalidSlot { slot: 6 },
            FlatPageError::KeysOutOfOrder { slot: 7 },
            FlatPageError::UnusedBytes { used: 80, len: 90 },
            FlatPageError::CapacityOutOfRange { capacity: 70_000 },
            FlatPageError::KeyOutsidePrefix,
            FlatPageError::PageFull {
                needed: 4100,
                capacity: 4096,
            },
        ];
        let messages: BTreeSet<String> = errors
            .iter()
            .map(|&error| FormatError::from(error).to_string())
            .collect();
        assert_eq!(messages.len(), errors.len());
        assert!(messages
            .iter()
            .all(|message| message.starts_with("flat page: ")));
    }
}
