//! The cell of one node, and the links in it.

use std::{fmt, iter::FusedIterator, slice};

use super::TreePageError;
use crate::{
    error::FormatError,
    header::FormatVersion,
    ids::{CellAddress, FileId},
    value_log::{PendingValue, ValuePlace, ValuePointer},
};

/// Bytes of a hash.
pub const HASH_LEN: usize = 32;

/// A hash as Merk stores it. The format does not interpret it.
pub type Hash = [u8; HASH_LEN];

const FEATURE_LEN_AT: usize = 2;
const HOSTED_COUNT_AT: usize = 3;
const VALUE_LEN_AT: usize = 4;
const VALUE_HASH_AT: usize = 8;
const KV_HASH_AT: usize = VALUE_HASH_AT + HASH_LEN;
const LINKS_AT: usize = KV_HASH_AT + HASH_LEN;

const OUT_OF_LINE: u8 = 1 << 2;
const FRAGMENT_ROOT: u8 = 1 << 3;
const DEFINED_FLAGS: u8 = 0x0F;

const LINK_HASH_AT: usize = CellAddress::ENCODED_LEN;
const LINK_HEIGHTS_AT: usize = LINK_HASH_AT + HASH_LEN;
const LINK_KEY_LEN_AT: usize = LINK_HEIGHTS_AT + 2;
const LINK_AGGREGATE_LEN_AT: usize = LINK_KEY_LEN_AT + 1;

const VALUE_OFFSET_LEN: usize = 8;
const MAX_FIELD_LEN: usize = u8::MAX as usize;
const MAX_LINK_LEN: usize = LINK_AGGREGATE_LEN_AT + 1 + MAX_FIELD_LEN;

/// Which child of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The child with the smaller keys.
    Left,
    /// The child with the larger keys.
    Right,
}

impl Side {
    /// Both sides, left first, which is the order of the links in a cell.
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    const fn flag(self) -> u8 {
        match self {
            Self::Left => 1 << 0,
            Self::Right => 1 << 1,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Left => "left",
            Self::Right => "right",
        })
    }
}

/// One of the hashes a cell stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashField {
    /// The node's value hash.
    Value,
    /// The node's key-value hash.
    KeyValue,
    /// The hash of the child on a side, in the link to it.
    Child(Side),
}

impl HashField {
    /// Every hash a cell can store.
    pub const ALL: [Self; 4] = [
        Self::Value,
        Self::KeyValue,
        Self::Child(Side::Left),
        Self::Child(Side::Right),
    ];
}

/// A part of a cell whose length is limited by its length field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellField {
    /// The node's key.
    Key,
    /// The node's feature type with its aggregate data.
    Feature,
    /// The aggregate data of a child, in the link to it.
    Aggregate,
    /// The pointers to the roots of the Merks the node hosts.
    HostedRoots,
    /// A value stored in the cell.
    InlineValue,
}

impl fmt::Display for CellField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Key => "key",
            Self::Feature => "feature",
            Self::Aggregate => "aggregate",
            Self::HostedRoots => "hosted-root list",
            Self::InlineValue => "inline value",
        })
    }
}

const fn check_field(field: CellField, len: usize, max: usize) -> Result<(), FormatError> {
    if len > max {
        return Err(FormatError::TreePage(TreePageError::FieldTooLong {
            field,
            len,
            max,
        }));
    }
    Ok(())
}

pub(super) fn inline_value_len(len: usize) -> Result<u32, FormatError> {
    let max = u32::MAX as usize;
    check_field(CellField::InlineValue, len, max)?;
    Ok(len as u32)
}

/// What a cell stores for the node's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellValue<'a> {
    /// The value itself.
    Inline(&'a [u8]),
    /// A pointer to the value in the value log.
    OutOfLine(ValuePointer),
    /// The value is out of line and not in the value log yet: what stands
    /// where its pointer will be.
    Pending(PendingValue),
}

impl<'a> CellValue<'a> {
    /// Whether the cell holds the value itself.
    pub const fn is_inline(&self) -> bool {
        matches!(self, Self::Inline(_))
    }

    /// The value, if the cell holds it.
    pub const fn inline(&self) -> Option<&'a [u8]> {
        match *self {
            Self::Inline(value) => Some(value),
            Self::OutOfLine(_) | Self::Pending(_) => None,
        }
    }

    /// The pointer to the value, if the value is in the value log.
    pub const fn pointer(&self) -> Option<ValuePointer> {
        match *self {
            Self::Inline(_) | Self::Pending(_) => None,
            Self::OutOfLine(pointer) => Some(pointer),
        }
    }

    /// The pending value, if the value is out of line and not in the value
    /// log yet.
    pub const fn pending(&self) -> Option<PendingValue> {
        match *self {
            Self::Inline(_) | Self::OutOfLine(_) => None,
            Self::Pending(pending) => Some(pending),
        }
    }

    /// Where the value is, if it is not in the cell.
    pub const fn place(&self) -> Option<ValuePlace> {
        match *self {
            Self::Inline(_) => None,
            Self::OutOfLine(pointer) => Some(ValuePlace::Placed(pointer)),
            Self::Pending(pending) => Some(ValuePlace::Pending(pending)),
        }
    }

    /// What a cell stores for a value that is at `place`.
    pub const fn of_place(place: ValuePlace) -> Self {
        match place {
            ValuePlace::Placed(pointer) => Self::OutOfLine(pointer),
            ValuePlace::Pending(pending) => Self::Pending(pending),
        }
    }

    /// The bytes the value takes in its cell: the value, or the place of a
    /// value out of line.
    pub const fn stored_len(&self) -> usize {
        match self {
            Self::Inline(value) => value.len(),
            Self::OutOfLine(_) | Self::Pending(_) => TreeCell::VALUE_LOCATION_LEN,
        }
    }

    fn value_len(&self) -> Result<u32, FormatError> {
        match self {
            Self::Inline(value) => inline_value_len(value.len()),
            Self::OutOfLine(pointer) => Ok(pointer.value_len()),
            Self::Pending(pending) => Ok(pending.value_len()),
        }
    }

    const fn flags(&self) -> u8 {
        match self {
            Self::Inline(_) => 0,
            Self::OutOfLine(_) | Self::Pending(_) => OUT_OF_LINE,
        }
    }

    fn write(&self, out: &mut Writer<'_>) {
        let place = match *self {
            Self::Inline(value) => return out.put(value),
            Self::OutOfLine(pointer) => ValuePlace::Placed(pointer),
            Self::Pending(pending) => ValuePlace::Pending(pending),
        };
        out.put(&place.encode()[..TreeCell::VALUE_LOCATION_LEN]);
    }
}

/// What a cell stores about one child: where the child's cell is, and what
/// Merk keeps about a child in its parent.
///
/// The hash and the aggregate are borrowed, from the page when the link was
/// read and from the caller when it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLink<'a> {
    /// The address of the child's cell.
    pub child: CellAddress,
    /// The child's hash.
    pub hash: &'a Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The length of the child's key. The key is in the child's cell.
    pub child_key_len: u8,
    /// The child's aggregate data, in the caller's encoding.
    pub aggregate: &'a [u8],
}

impl TreeLink<'_> {
    /// Bytes of a link before its aggregate.
    pub const FIXED_LEN: usize = LINK_AGGREGATE_LEN_AT + 1;
    /// The longest aggregate of a link.
    pub const MAX_AGGREGATE_LEN: usize = MAX_FIELD_LEN;
    /// The longest link.
    pub const MAX_LEN: usize = MAX_LINK_LEN;

    /// Bytes the link takes in its cell.
    pub const fn encoded_len(&self) -> usize {
        Self::FIXED_LEN + self.aggregate.len()
    }

    const fn check(&self) -> Result<(), FormatError> {
        check_field(
            CellField::Aggregate,
            self.aggregate.len(),
            Self::MAX_AGGREGATE_LEN,
        )
    }

    fn write(&self, out: &mut Writer<'_>) {
        out.put(&self.child.encode());
        out.put(self.hash);
        out.put(&[
            self.heights.0,
            self.heights.1,
            self.child_key_len,
            self.aggregate.len() as u8,
        ]);
        out.put(self.aggregate);
    }

    /// Writes the link into `buffer` and returns its bytes.
    pub(super) fn checked_write<'b>(
        &self,
        buffer: &'b mut [u8; MAX_LINK_LEN],
    ) -> Result<&'b [u8], FormatError> {
        self.check()?;
        let bytes = &mut buffer[..self.encoded_len()];
        self.write(&mut Writer(bytes));
        Ok(bytes)
    }
}

/// The parts of one node's cell, as a writer gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellContent<'a> {
    /// The node's key.
    pub key: &'a [u8],
    /// The node's feature type with its aggregate data, in the caller's
    /// encoding.
    pub feature: &'a [u8],
    /// The node's value hash.
    pub value_hash: &'a Hash,
    /// The node's key-value hash.
    pub kv_hash: &'a Hash,
    /// The link to the left child.
    pub left: Option<TreeLink<'a>>,
    /// The link to the right child.
    pub right: Option<TreeLink<'a>>,
    /// The roots of the Merks the node hosts, by position. `None` is a
    /// hosted Merk with no root.
    pub hosted_roots: &'a [Option<CellAddress>],
    /// The node's value, or the pointer to it.
    pub value: CellValue<'a>,
    /// Whether the cell is the root of a fragment: no cell of its page
    /// points at it.
    pub fragment_root: bool,
}

impl<'a> CellContent<'a> {
    /// The link on a side.
    pub const fn link(&self, side: Side) -> Option<&TreeLink<'a>> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }

    /// Bytes the cell takes in a page, without its slot.
    pub fn encoded_len(&self) -> usize {
        TreeCell::len_of(
            self.key.len(),
            self.feature.len(),
            self.left.map(|link| link.aggregate.len()),
            self.right.map(|link| link.aggregate.len()),
            self.hosted_roots.len(),
            self.value.stored_len(),
        )
    }

    /// The cell's bytes.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        let mut bytes = Vec::new();
        self.encode_into(&mut bytes)?;
        Ok(bytes)
    }

    /// Appends the cell's bytes to `out`. On an error `out` is unchanged.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let len = self.checked_len()?;
        let start = out.len();
        out.resize(start + len, 0);
        self.write(&mut out[start..]);
        Ok(())
    }

    pub(super) fn checked_len(&self) -> Result<usize, FormatError> {
        check_field(CellField::Key, self.key.len(), TreeCell::MAX_KEY_LEN)?;
        check_field(
            CellField::Feature,
            self.feature.len(),
            TreeCell::MAX_FEATURE_LEN,
        )?;
        check_field(
            CellField::HostedRoots,
            self.hosted_roots.len(),
            TreeCell::MAX_HOSTED_ROOTS,
        )?;
        for side in Side::BOTH {
            if let Some(link) = self.link(side) {
                link.check()?;
            }
        }
        self.value.value_len()?;
        Ok(self.encoded_len())
    }

    pub(super) fn write(&self, out: &mut [u8]) {
        let mut flags = self.value.flags();
        for side in Side::BOTH {
            if self.link(side).is_some() {
                flags |= side.flag();
            }
        }
        if self.fragment_root {
            flags |= FRAGMENT_ROOT;
        }
        let value_len = self.value.value_len().unwrap_or_default();
        let mut out = Writer(out);
        out.put(&[
            flags,
            self.key.len() as u8,
            self.feature.len() as u8,
            self.hosted_roots.len() as u8,
        ]);
        out.put(&value_len.to_le_bytes());
        out.put(self.value_hash);
        out.put(self.kv_hash);
        for side in Side::BOTH {
            if let Some(link) = self.link(side) {
                link.write(&mut out);
            }
        }
        for root in self.hosted_roots {
            out.put(&CellAddress::encode_optional(*root));
        }
        out.put(self.feature);
        out.put(self.key);
        self.value.write(&mut out);
    }
}

struct Writer<'a>(&'a mut [u8]);

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        let (head, rest) = std::mem::take(&mut self.0).split_at_mut(bytes.len());
        head.copy_from_slice(bytes);
        self.0 = rest;
    }
}

#[derive(Debug, Clone, Copy)]
struct LinkAt {
    at: usize,
    len: usize,
    child: CellAddress,
}

#[derive(Debug, Clone, Copy)]
struct Layout {
    flags: u8,
    value_len: u32,
    links: [Option<LinkAt>; 2],
    hosted_at: usize,
    feature_at: usize,
    key_at: usize,
    value_at: usize,
    end: usize,
    place: Option<ValuePlace>,
}

impl Layout {
    fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let truncated = |needed| FormatError::Truncated {
            needed,
            available: bytes.len(),
        };
        let Some(&[flags, key_len, feature_len, hosted_count, v0, v1, v2, v3, ..]) =
            bytes.first_chunk::<LINKS_AT>()
        else {
            return Err(truncated(LINKS_AT));
        };
        if flags & !DEFINED_FLAGS != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        let value_len = u32::from_le_bytes([v0, v1, v2, v3]);
        let mut at = LINKS_AT;
        let mut links = [None; 2];
        for (side, link) in Side::BOTH.into_iter().zip(&mut links) {
            if flags & side.flag() == 0 {
                continue;
            }
            let needed = at + TreeLink::FIXED_LEN;
            let Some((address, rest)) = bytes
                .get(at..needed)
                .and_then(<[u8]>::split_first_chunk::<{ CellAddress::ENCODED_LEN }>)
            else {
                return Err(truncated(needed));
            };
            let child = CellAddress::decode(*address)?.ok_or(FormatError::MissingId)?;
            let aggregate_len = rest.last().copied().unwrap_or_default();
            let len = TreeLink::FIXED_LEN + usize::from(aggregate_len);
            *link = Some(LinkAt { at, len, child });
            at += len;
        }
        let hosted_at = at;
        let feature_at = hosted_at + usize::from(hosted_count) * TreeCell::HOSTED_ROOT_LEN;
        let key_at = feature_at + usize::from(feature_len);
        let value_at = key_at + usize::from(key_len);
        let (place, stored_len) = if flags & OUT_OF_LINE == 0 {
            (None, usize::try_from(value_len).unwrap_or(usize::MAX))
        } else {
            let needed = value_at + TreeCell::VALUE_LOCATION_LEN;
            let Some(location) = bytes
                .get(value_at..needed)
                .and_then(<[u8]>::first_chunk::<{ TreeCell::VALUE_LOCATION_LEN }>)
            else {
                return Err(truncated(needed));
            };
            let mut place = [0; ValuePlace::ENCODED_LEN];
            place[..TreeCell::VALUE_LOCATION_LEN].copy_from_slice(location);
            place[TreeCell::VALUE_LOCATION_LEN..].copy_from_slice(&value_len.to_le_bytes());
            let place = ValuePlace::decode(place)?;
            (Some(place), TreeCell::VALUE_LOCATION_LEN)
        };
        let end = value_at.saturating_add(stored_len);
        if bytes.len() < end {
            return Err(truncated(end));
        }
        Ok(Self {
            flags,
            value_len,
            links,
            hosted_at,
            feature_at,
            key_at,
            value_at,
            end,
            place,
        })
    }

    const fn link(&self, side: Side) -> Option<LinkAt> {
        self.links[side as usize]
    }
}

/// The cell of one node, borrowed from a page or from its own bytes.
///
/// No accessor allocates, and none reads the value bytes unless it returns
/// them.
#[derive(Debug, Clone, Copy)]
pub struct TreeCell<'a> {
    bytes: &'a [u8],
    layout: Layout,
}

/// Two cells are equal when their bytes are: everything a cell holds is in
/// them.
impl PartialEq for TreeCell<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for TreeCell<'_> {}

impl<'a> TreeCell<'a> {
    /// Bytes of a cell before its links: the header and the node's two
    /// hashes.
    pub const FIXED_LEN: usize = LINKS_AT;
    /// Bytes of the header of a cell: flags and lengths.
    pub const HEADER_LEN: usize = VALUE_HASH_AT;
    /// Bytes of one hosted-root pointer.
    pub const HOSTED_ROOT_LEN: usize = CellAddress::ENCODED_LEN;
    /// Bytes a cell stores in place of a value that is out of line: the file
    /// id and the offset of the value pointer, or the reserved file id and
    /// the id of a pending value. The value length of either is the cell's
    /// own value length.
    pub const VALUE_LOCATION_LEN: usize = FileId::ENCODED_LEN + VALUE_OFFSET_LEN;
    /// The longest key.
    pub const MAX_KEY_LEN: usize = MAX_FIELD_LEN;
    /// The longest feature.
    pub const MAX_FEATURE_LEN: usize = MAX_FIELD_LEN;
    /// The most hosted-root pointers of one cell.
    pub const MAX_HOSTED_ROOTS: usize = MAX_FIELD_LEN;
    /// The longest cell whose value is out of line, whatever else it holds.
    pub const MAX_OUT_OF_LINE_LEN: usize = Self::len_of(
        Self::MAX_KEY_LEN,
        Self::MAX_FEATURE_LEN,
        Some(TreeLink::MAX_AGGREGATE_LEN),
        Some(TreeLink::MAX_AGGREGATE_LEN),
        Self::MAX_HOSTED_ROOTS,
        Self::VALUE_LOCATION_LEN,
    );

    /// Bytes a cell takes in a page, without its slot.
    ///
    /// The aggregate lengths are those of the links, `None` for a side with
    /// no child. `stored_value_len` is the length of a value in the cell, or
    /// [`Self::VALUE_LOCATION_LEN`] for a value out of line.
    pub const fn len_of(
        key_len: usize,
        feature_len: usize,
        left_aggregate_len: Option<usize>,
        right_aggregate_len: Option<usize>,
        hosted_roots: usize,
        stored_value_len: usize,
    ) -> usize {
        const fn link_len(aggregate_len: Option<usize>) -> usize {
            match aggregate_len {
                Some(aggregate_len) => TreeLink::FIXED_LEN.saturating_add(aggregate_len),
                None => 0,
            }
        }
        Self::FIXED_LEN
            .saturating_add(link_len(left_aggregate_len))
            .saturating_add(link_len(right_aggregate_len))
            .saturating_add(hosted_roots.saturating_mul(Self::HOSTED_ROOT_LEN))
            .saturating_add(feature_len)
            .saturating_add(key_len)
            .saturating_add(stored_value_len)
    }

    /// Reads and fully checks a cell that is exactly `bytes`, written in
    /// format `version`.
    ///
    /// The checks run in a fixed order: the fixed part, the reserved flags,
    /// each link's address, the value pointer, that all of the cell is
    /// there, each hosted-root pointer, then that nothing follows the cell.
    pub fn decode(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let cell = Self::at_start(bytes, version)?;
        cell.check_hosted_roots()?;
        if cell.bytes.len() != bytes.len() {
            return Err(TreePageError::CellLengthMismatch {
                expected: cell.bytes.len(),
                found: bytes.len(),
            }
            .into());
        }
        Ok(cell)
    }

    pub(super) fn at_start(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let layout = match version {
            FormatVersion::V1 => Layout::parse(bytes)?,
        };
        Ok(Self {
            bytes: bytes.get(..layout.end).unwrap_or(bytes),
            layout,
        })
    }

    pub(super) fn check_hosted_roots(&self) -> Result<(), FormatError> {
        self.hosted_root_fields()
            .iter()
            .try_for_each(|field| CellAddress::decode(*field).map(drop))
    }

    fn part(&self, start: usize, end: usize) -> &'a [u8] {
        self.bytes.get(start..end).unwrap_or_default()
    }

    fn hosted_root_fields(&self) -> &'a [[u8; CellAddress::ENCODED_LEN]] {
        self.part(self.layout.hosted_at, self.layout.feature_at)
            .as_chunks()
            .0
    }

    /// The cell's bytes.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Bytes the cell takes in a page, without its slot.
    pub const fn encoded_len(&self) -> usize {
        self.bytes.len()
    }

    /// The node's key.
    pub fn key(&self) -> &'a [u8] {
        self.part(self.layout.key_at, self.layout.value_at)
    }

    /// The node's feature type with its aggregate data, in the writer's
    /// encoding.
    pub fn feature(&self) -> &'a [u8] {
        self.part(self.layout.feature_at, self.layout.key_at)
    }

    /// The length of the node's value, wherever the value is.
    pub const fn value_len(&self) -> u32 {
        self.layout.value_len
    }

    /// The node's value, the pointer to it, or what stands for a value that
    /// is not in the value log yet.
    pub fn value(&self) -> CellValue<'a> {
        match self.layout.place {
            Some(place) => CellValue::of_place(place),
            None => CellValue::Inline(self.part(self.layout.value_at, self.layout.end)),
        }
    }

    /// The pointer to the node's value, if the value is in the value log.
    /// `None` for a value in the cell and for a pending one.
    pub const fn value_pointer(&self) -> Option<ValuePointer> {
        match self.layout.place {
            Some(place) => place.pointer(),
            None => None,
        }
    }

    /// The pending value, if the node's value is out of line and not in
    /// the value log yet.
    pub const fn pending_value(&self) -> Option<PendingValue> {
        match self.layout.place {
            Some(place) => place.pending(),
            None => None,
        }
    }

    /// Where the node's value is, if it is not in the cell.
    pub const fn value_place(&self) -> Option<ValuePlace> {
        self.layout.place
    }

    /// Whether the cell is marked as the root of a fragment.
    pub const fn is_fragment_root(&self) -> bool {
        self.layout.flags & FRAGMENT_ROOT != 0
    }

    /// One of the hashes the cell stores. `None` for the hash of a child the
    /// node does not have.
    pub fn hash(&self, field: HashField) -> Option<&'a Hash> {
        self.bytes.get(self.hash_at(field).ok()?..)?.first_chunk()
    }

    /// The node's value hash.
    pub fn value_hash(&self) -> &'a Hash {
        self.hash(HashField::Value).unwrap_or(&[0; HASH_LEN])
    }

    /// The node's key-value hash.
    pub fn kv_hash(&self) -> &'a Hash {
        self.hash(HashField::KeyValue).unwrap_or(&[0; HASH_LEN])
    }

    /// Whether the node has a child on a side.
    pub const fn has_link(&self, side: Side) -> bool {
        self.layout.link(side).is_some()
    }

    /// The link to the child on a side.
    pub fn link(&self, side: Side) -> Option<TreeLink<'a>> {
        let LinkAt { at, len, child } = self.layout.link(side)?;
        let (fixed, aggregate) = self
            .part(at, at + len)
            .split_first_chunk::<{ TreeLink::FIXED_LEN }>()?;
        let (_, &[left_height, right_height, child_key_len, _]) = fixed.split_last_chunk()?;
        Some(TreeLink {
            child,
            hash: fixed.get(LINK_HASH_AT..)?.first_chunk()?,
            heights: (left_height, right_height),
            child_key_len,
            aggregate,
        })
    }

    /// How many hosted-root pointers the cell has.
    pub fn hosted_root_count(&self) -> usize {
        self.hosted_root_fields().len()
    }

    /// The roots of the Merks the node hosts, by position. `None` is a
    /// hosted Merk with no root.
    pub fn hosted_roots(&self) -> HostedRoots<'a> {
        HostedRoots(self.hosted_root_fields().iter())
    }

    pub(super) const fn hash_at(&self, field: HashField) -> Result<usize, Side> {
        match field {
            HashField::Value => Ok(VALUE_HASH_AT),
            HashField::KeyValue => Ok(KV_HASH_AT),
            HashField::Child(side) => match self.layout.link(side) {
                Some(link) => Ok(link.at + LINK_HASH_AT),
                None => Err(side),
            },
        }
    }

    pub(super) const fn link_extent(&self, side: Side) -> (usize, usize) {
        match (self.layout.link(side), side) {
            (Some(link), _) => (link.at, link.at + link.len),
            (None, Side::Left) => (LINKS_AT, LINKS_AT),
            (None, Side::Right) => match self.layout.link(Side::Left) {
                Some(left) => (left.at + left.len, left.at + left.len),
                None => (LINKS_AT, LINKS_AT),
            },
        }
    }

    pub(super) const fn hosted_roots_extent(&self) -> (usize, usize) {
        (self.layout.hosted_at, self.layout.feature_at)
    }

    pub(super) const fn feature_extent(&self) -> (usize, usize) {
        (self.layout.feature_at, self.layout.key_at)
    }

    pub(super) const fn value_extent(&self) -> (usize, usize) {
        (self.layout.value_at, self.layout.end)
    }
}

/// The hosted-root pointers of a cell, by position.
#[derive(Debug, Clone)]
pub struct HostedRoots<'a>(slice::Iter<'a, [u8; CellAddress::ENCODED_LEN]>);

impl HostedRoots<'_> {
    fn read(field: &[u8; CellAddress::ENCODED_LEN]) -> Option<CellAddress> {
        CellAddress::decode(*field).ok().flatten()
    }
}

impl Iterator for HostedRoots<'_> {
    type Item = Option<CellAddress>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(Self::read)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.0.nth(n).map(Self::read)
    }
}

impl DoubleEndedIterator for HostedRoots<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(Self::read)
    }
}

impl ExactSizeIterator for HostedRoots<'_> {}

impl FusedIterator for HostedRoots<'_> {}

/// Patches of a cell's own bytes. Each takes the bytes of one whole cell.
pub(super) mod patch {
    use super::{
        CellValue, Side, FEATURE_LEN_AT, FRAGMENT_ROOT, HOSTED_COUNT_AT, OUT_OF_LINE, VALUE_LEN_AT,
    };

    pub(in super::super) fn set_fragment_root(cell: &mut [u8], fragment_root: bool) {
        set_flag(cell, FRAGMENT_ROOT, fragment_root);
    }

    pub(in super::super) fn set_link_present(cell: &mut [u8], side: Side, present: bool) {
        set_flag(cell, side.flag(), present);
    }

    pub(in super::super) fn set_feature_len(cell: &mut [u8], len: usize) {
        cell[FEATURE_LEN_AT] = len as u8;
    }

    pub(in super::super) fn set_hosted_root_count(cell: &mut [u8], count: usize) {
        cell[HOSTED_COUNT_AT] = count as u8;
    }

    pub(in super::super) fn set_value_header(cell: &mut [u8], value: &CellValue<'_>, len: u32) {
        set_flag(cell, OUT_OF_LINE, !value.is_inline());
        cell[VALUE_LEN_AT..VALUE_LEN_AT + 4].copy_from_slice(&len.to_le_bytes());
    }

    fn set_flag(cell: &mut [u8], flag: u8, on: bool) {
        if on {
            cell[0] |= flag;
        } else {
            cell[0] &= !flag;
        }
    }
}

/// The bytes a cell stores for `value`, and the value's length. A value out
/// of line is written into `location`.
pub(super) fn stored_value<'a>(
    value: &CellValue<'a>,
    location: &'a mut [u8; TreeCell::VALUE_LOCATION_LEN],
) -> Result<(&'a [u8], u32), FormatError> {
    let len = value.value_len()?;
    Ok(match value {
        CellValue::Inline(value) => (value, len),
        CellValue::OutOfLine(_) | CellValue::Pending(_) => {
            value.write(&mut Writer(&mut location[..]));
            (location, len)
        }
    })
}

pub(super) fn check_feature(feature: &[u8]) -> Result<(), FormatError> {
    check_field(CellField::Feature, feature.len(), TreeCell::MAX_FEATURE_LEN)
}

/// Writes the hosted-root pointers `roots` into `buffer` and returns their
/// bytes.
pub(super) fn hosted_root_bytes<'b>(
    roots: &[Option<CellAddress>],
    buffer: &'b mut [u8; TreeCell::MAX_HOSTED_ROOTS * TreeCell::HOSTED_ROOT_LEN],
) -> Result<&'b [u8], FormatError> {
    check_field(
        CellField::HostedRoots,
        roots.len(),
        TreeCell::MAX_HOSTED_ROOTS,
    )?;
    let bytes = &mut buffer[..roots.len() * TreeCell::HOSTED_ROOT_LEN];
    let mut out = Writer(bytes);
    for root in roots {
        out.put(&CellAddress::encode_optional(*root));
    }
    Ok(bytes)
}
