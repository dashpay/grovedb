//! The value log: records for values that are kept outside their cell.
//!
//! See `docs/lignindb/value-log-format.md`.
//!
//! A value-log file is the file header followed by values. A value is one
//! record, or several back to back when it is longer than one checksum may
//! cover. Each record is a default frame around ten bytes of fields and the
//! value's bytes for that part:
//!
//! ```text
//! offset  size  field
//!      0     4  payload length: 10 + n
//!      4     4  checksum of bytes 0..4 and 8..18+n
//!      8     4  owner: the page id of the page whose cell points at the value
//!     12     2  part: the record's place among the value's records, from 0
//!     14     4  value length: bytes of the whole value
//!     18     n  the value's bytes for this part
//! ```
//!
//! The owner is the back pointer. The cell is the one in that page that
//! holds the [`ValuePointer`] to the value.
//!
//! A cell whose value is not in the value log yet holds a [`PendingValue`]
//! where the pointer will be: a place that names no file. [`ValuePlace`]
//! is either of the two.

use std::{fmt, slice};

use crate::{
    checksum::Checksum,
    error::FormatError,
    flat_page::{FlatLeaf, FlatValue},
    header::{read_record, FileHeader, FileKind, FormatVersion, RecordHeader},
    ids::{FileId, PageId},
};

const FIELDS_LEN: usize = PageId::ENCODED_LEN + 2 + 4;

/// Bytes of a record in front of the value's bytes: the frame and the fields.
pub const RECORD_HEADER_LEN: usize = RecordHeader::ENCODED_LEN + FIELDS_LEN;

/// The most value bytes one record carries.
///
/// It is what one checksum may cover, less the record's own fields. A longer
/// value is split into parts of exactly this length, and a last part with
/// what is left.
pub const MAX_PART_LEN: usize = RecordHeader::MAX_PAYLOAD_LEN - FIELDS_LEN;

/// The longest value the value log stores.
pub const MAX_VALUE_LEN: usize = u32::MAX as usize;

/// The offset of the first record in a value-log file.
pub const FIRST_RECORD_OFFSET: u64 = FileHeader::ENCODED_LEN as u64;

/// The largest offset a value may start at: the largest a file API takes.
pub const MAX_VALUE_OFFSET: u64 = i64::MAX as u64;

const MAX_PARTS: u64 = 1 << 16;

/// Why bytes are not a valid value-log encoding, or a value cannot be
/// encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueLogError {
    /// The file is not a value log.
    WrongFileKind {
        /// The kind in the file header.
        found: FileKind,
    },
    /// A record's payload is shorter than the fields every record carries.
    RecordTooShort {
        /// The payload length.
        len: usize,
    },
    /// A value of that length has no part with that number.
    NoSuchPart {
        /// The part number in the record.
        part: u16,
        /// The value length in the record.
        value_len: u32,
    },
    /// A record does not carry the number of bytes its part of the value has.
    WrongPartLength {
        /// The bytes that part of the value has.
        expected: usize,
        /// The bytes the record carries.
        found: usize,
    },
    /// A record is not the part of the value that belongs at its place.
    UnexpectedPart {
        /// The part that belongs there.
        expected: u16,
        /// The part number in the record.
        found: u16,
    },
    /// A later part names another owner or value length than the first part.
    PartOfAnotherValue,
    /// The value at a pointer does not have the length the pointer states.
    WrongValueLength {
        /// The value length in the pointer.
        expected: u32,
        /// The value length in the record.
        found: u32,
    },
    /// The value at a pointer is owned by another page than the one whose
    /// cell holds the pointer.
    WrongOwner {
        /// The page whose cell holds the pointer.
        expected: PageId,
        /// The owner in the record.
        found: PageId,
    },
    /// An empty value has no record.
    EmptyValue,
    /// The value is longer than the value log stores.
    ValueTooLong {
        /// The value's length.
        len: u64,
    },
    /// A value cannot start at that offset of a file.
    InvalidOffset {
        /// The offset.
        offset: u64,
    },
}

impl fmt::Display for ValueLogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongFileKind { found } => write!(f, "a {found:?} file is not a value log"),
            Self::RecordTooShort { len } => write!(
                f,
                "record payload of {len} bytes is shorter than its {FIELDS_LEN} bytes of fields"
            ),
            Self::NoSuchPart { part, value_len } => {
                write!(f, "a value of {value_len} bytes has no part {part}")
            }
            Self::WrongPartLength { expected, found } => {
                write!(f, "part carries {found} bytes, expected {expected}")
            }
            Self::UnexpectedPart { expected, found } => {
                write!(f, "found part {found} where part {expected} belongs")
            }
            Self::PartOfAnotherValue => {
                write!(f, "part names another owner or value length than part 0")
            }
            Self::WrongValueLength { expected, found } => write!(
                f,
                "pointer states a value of {expected} bytes, the record one of {found}"
            ),
            Self::WrongOwner { expected, found } => {
                write!(f, "value is owned by page {found}, not by page {expected}")
            }
            Self::EmptyValue => write!(f, "an empty value has no record"),
            Self::ValueTooLong { len } => {
                write!(
                    f,
                    "value of {len} bytes is longer than the value log stores"
                )
            }
            Self::InvalidOffset { offset } => write!(f, "no value starts at offset {offset}"),
        }
    }
}

impl std::error::Error for ValueLogError {}

impl From<ValueLogError> for FormatError {
    fn from(error: ValueLogError) -> Self {
        Self::ValueLog(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    max_part_len: u32,
}

impl Layout {
    const V1: Self = Self {
        max_part_len: MAX_PART_LEN as u32,
    };

    const fn of(version: FormatVersion) -> Self {
        match version {
            FormatVersion::V1 => Self::V1,
        }
    }

    const fn max_payload_len(self) -> usize {
        FIELDS_LEN + to_usize(self.max_part_len)
    }

    fn max_value_len(self) -> u64 {
        (u64::from(self.max_part_len) * MAX_PARTS).min(u64::from(u32::MAX))
    }

    fn checked_value_len(self, len: usize) -> Result<u32, ValueLogError> {
        if len == 0 {
            return Err(ValueLogError::EmptyValue);
        }
        let too_long = ValueLogError::ValueTooLong { len: len as u64 };
        let value_len = u32::try_from(len).map_err(|_| too_long)?;
        if u64::from(value_len) > self.max_value_len() {
            return Err(too_long);
        }
        Ok(value_len)
    }

    const fn part_count(self, value_len: u32) -> u32 {
        value_len.div_ceil(self.max_part_len)
    }

    fn part_len(self, value_len: u32, part: u16) -> Option<u32> {
        if u64::from(value_len) > self.max_value_len() {
            return None;
        }
        let before = u32::try_from(u64::from(part) * u64::from(self.max_part_len)).ok()?;
        let left = value_len.checked_sub(before).filter(|&left| left != 0)?;
        Some(left.min(self.max_part_len))
    }

    fn extent_len(self, value_len: u32) -> u64 {
        u64::from(value_len) + RECORD_HEADER_LEN as u64 * u64::from(self.part_count(value_len))
    }
}

const fn to_usize(len: u32) -> usize {
    len as usize
}

/// Checks that a value of `len` bytes can be stored, and returns the length
/// as records and pointers hold it.
pub fn checked_value_len(len: usize) -> Result<u32, FormatError> {
    Ok(Layout::of(FormatVersion::CURRENT).checked_value_len(len)?)
}

/// The bytes the records of a value of `value_len` bytes take in a file of
/// format `version`: the value and one record header for each part.
pub fn extent_len(value_len: u32, version: FormatVersion) -> u64 {
    Layout::of(version).extent_len(value_len)
}

/// The length of the record that starts with `bytes`, read from its first
/// [`RecordHeader::ENCODED_LEN`] bytes.
///
/// A reader that fetches a file piece by piece uses it to learn how much to
/// fetch. Nothing else about the record is known until it is decoded.
pub fn record_len(bytes: &[u8], version: FormatVersion) -> Result<usize, FormatError> {
    let frame = RecordHeader::decode(bytes, Layout::of(version).max_payload_len())?;
    Ok(RecordHeader::ENCODED_LEN + frame.payload_len())
}

/// One record of a value log, decoded and verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueRecord<'a> {
    owner: PageId,
    part: u16,
    value_len: u32,
    data: &'a [u8],
}

impl<'a> ValueRecord<'a> {
    /// Reads the record at the start of `bytes`, from a file of format
    /// `version`.
    ///
    /// Returns the record and the bytes after it. The checks run in a fixed
    /// order: frame length, bytes available, checksum, fields present, owner,
    /// part number, part length.
    pub fn decode(
        bytes: &'a [u8],
        version: FormatVersion,
    ) -> Result<(Self, &'a [u8]), FormatError> {
        Self::decode_with(bytes, Layout::of(version))
    }

    fn decode_with(bytes: &'a [u8], layout: Layout) -> Result<(Self, &'a [u8]), FormatError> {
        let (payload, rest) = read_record(bytes, layout.max_payload_len())?;
        let Some((fields, data)) = payload.split_first_chunk::<FIELDS_LEN>() else {
            return Err(ValueLogError::RecordTooShort { len: payload.len() }.into());
        };
        let [p0, p1, p2, p3, i0, i1, l0, l1, l2, l3] = *fields;
        let owner = PageId::decode([p0, p1, p2, p3]).ok_or(FormatError::MissingId)?;
        let part = u16::from_le_bytes([i0, i1]);
        let value_len = u32::from_le_bytes([l0, l1, l2, l3]);

        let expected = layout
            .part_len(value_len, part)
            .map(to_usize)
            .ok_or(ValueLogError::NoSuchPart { part, value_len })?;
        if data.len() != expected {
            return Err(ValueLogError::WrongPartLength {
                expected,
                found: data.len(),
            }
            .into());
        }
        let record = Self {
            owner,
            part,
            value_len,
            data,
        };
        Ok((record, rest))
    }

    /// The page whose cell owns the value.
    pub const fn owner(&self) -> PageId {
        self.owner
    }

    /// The record's place among the value's records, from 0.
    pub const fn part(&self) -> u16 {
        self.part
    }

    /// The length of the whole value in bytes.
    pub const fn value_len(&self) -> u32 {
        self.value_len
    }

    /// The value's bytes for this part.
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Bytes the record takes in the file.
    pub const fn encoded_len(&self) -> usize {
        RECORD_HEADER_LEN + self.data.len()
    }
}

fn record_header(owner: PageId, part: u16, value_len: u32, data: &[u8]) -> [u8; RECORD_HEADER_LEN] {
    let [p0, p1, p2, p3] = owner.encode();
    let [i0, i1] = part.to_le_bytes();
    let [l0, l1, l2, l3] = value_len.to_le_bytes();
    let fields = [p0, p1, p2, p3, i0, i1, l0, l1, l2, l3];

    let payload_len = ((FIELDS_LEN + data.len()) as u32).to_le_bytes();
    let [n0, n1, n2, n3] = payload_len;
    let [c0, c1, c2, c3] = Checksum::of(&[&payload_len, &fields, data]).encode();
    [
        n0, n1, n2, n3, c0, c1, c2, c3, p0, p1, p2, p3, i0, i1, l0, l1, l2, l3,
    ]
}

/// The records of one value, ready to be written: each item is a record's
/// header and the slice of the value that follows it in the file.
///
/// Returned by [`encode_value`].
#[derive(Debug, Clone)]
pub struct ValueParts<'a> {
    owner: PageId,
    value_len: u32,
    part: u16,
    chunks: slice::Chunks<'a, u8>,
}

impl<'a> Iterator for ValueParts<'a> {
    type Item = ([u8; RECORD_HEADER_LEN], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let data = self.chunks.next()?;
        let header = record_header(self.owner, self.part, self.value_len, data);
        self.part = self.part.wrapping_add(1);
        Some((header, data))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}

impl ExactSizeIterator for ValueParts<'_> {}

/// Splits `value` into the records that store it for a cell of the page
/// `owner`.
///
/// The records go into one file back to back, in the order the iterator
/// gives them. The value's bytes are not copied: a writer can hand each
/// header and slice to a vectored write.
pub fn encode_value(owner: PageId, value: &[u8]) -> Result<ValueParts<'_>, FormatError> {
    encode_value_with(owner, value, Layout::of(FormatVersion::CURRENT))
}

fn encode_value_with(
    owner: PageId,
    value: &[u8],
    layout: Layout,
) -> Result<ValueParts<'_>, FormatError> {
    Ok(ValueParts {
        owner,
        value_len: layout.checked_value_len(value.len())?,
        part: 0,
        chunks: value.chunks(to_usize(layout.max_part_len)),
    })
}

/// Appends the records of `value`, owned by a cell of the page `owner`, to
/// `out`.
pub fn write_value(owner: PageId, value: &[u8], out: &mut Vec<u8>) -> Result<(), FormatError> {
    write_value_with(owner, value, out, Layout::of(FormatVersion::CURRENT))
}

fn write_value_with(
    owner: PageId,
    value: &[u8],
    out: &mut Vec<u8>,
    layout: Layout,
) -> Result<(), FormatError> {
    let parts = encode_value_with(owner, value, layout)?;
    out.reserve(value.len() + RECORD_HEADER_LEN * parts.len());
    for (header, data) in parts {
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
    }
    Ok(())
}

/// A whole value, read from its records and verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value<'a> {
    owner: PageId,
    value_len: u32,
    records: &'a [u8],
    layout: Layout,
}

impl<'a> Value<'a> {
    /// The page whose cell owns the value.
    pub const fn owner(self) -> PageId {
        self.owner
    }

    /// The value's length in bytes.
    pub const fn value_len(self) -> u32 {
        self.value_len
    }

    /// Bytes the value's records take in the file.
    pub const fn encoded_len(self) -> usize {
        self.records.len()
    }

    /// The value's bytes in order, one slice for each record.
    pub fn parts(self) -> impl Iterator<Item = &'a [u8]> {
        self.records
            .chunks(RECORD_HEADER_LEN + to_usize(self.layout.max_part_len))
            .map(|record| record.get(RECORD_HEADER_LEN..).unwrap_or_default())
    }

    /// The value's bytes as one slice, when the value is a single record.
    pub fn contiguous(self) -> Option<&'a [u8]> {
        let mut parts = self.parts();
        let first = parts.next()?;
        parts.next().is_none().then_some(first)
    }

    /// The value's bytes, copied into one vector.
    pub fn to_vec(self) -> Vec<u8> {
        let mut value = Vec::with_capacity(to_usize(self.value_len));
        self.parts().for_each(|part| value.extend_from_slice(part));
        value
    }
}

/// Reads the value whose first record starts `bytes`, from a file of format
/// `version`.
///
/// Returns the value and the bytes after its last record. Every record of the
/// value is verified. On [`FormatError::Truncated`] both counts start at the
/// first byte of `bytes`.
pub fn read_value(bytes: &[u8], version: FormatVersion) -> Result<(Value<'_>, &[u8]), FormatError> {
    read_value_with(bytes, Layout::of(version))
}

fn read_value_with(bytes: &[u8], layout: Layout) -> Result<(Value<'_>, &[u8]), FormatError> {
    let (first, rest) = ValueRecord::decode_with(bytes, layout)?;
    finish_value(bytes, first, rest, layout)
}

fn finish_value<'a>(
    bytes: &'a [u8],
    first: ValueRecord<'a>,
    mut rest: &'a [u8],
    layout: Layout,
) -> Result<(Value<'a>, &'a [u8]), FormatError> {
    expect_part(0, first.part)?;
    let later_parts = layout.part_count(first.value_len).saturating_sub(1);
    for expected in (1..=u16::MAX).take(to_usize(later_parts)) {
        let consumed = bytes.len() - rest.len();
        let (record, after) = ValueRecord::decode_with(rest, layout)
            .map_err(|error| counted_from_the_start(error, consumed))?;
        expect_part(expected, record.part)?;
        if record.owner != first.owner || record.value_len != first.value_len {
            return Err(ValueLogError::PartOfAnotherValue.into());
        }
        rest = after;
    }
    let value = Value {
        owner: first.owner,
        value_len: first.value_len,
        records: &bytes[..bytes.len() - rest.len()],
        layout,
    };
    Ok((value, rest))
}

fn expect_part(expected: u16, found: u16) -> Result<(), ValueLogError> {
    if found == expected {
        Ok(())
    } else {
        Err(ValueLogError::UnexpectedPart { expected, found })
    }
}

fn counted_from_the_start(error: FormatError, consumed: usize) -> FormatError {
    match error {
        FormatError::Truncated { needed, available } => FormatError::Truncated {
            needed: consumed.saturating_add(needed),
            available: consumed.saturating_add(available),
        },
        other => other,
    }
}

/// Where a value is in the value log: what a cell stores in place of the
/// value.
///
/// It names the file, the offset of the value's first record, and the
/// value's length. That is enough to fetch every record of the value with one
/// positional read of [`Self::extent_len`] bytes.
///
/// ```text
/// offset  size  field
///      0     4  file id
///      4     8  offset of the value's first record
///     12     4  value length
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValuePointer {
    file: FileId,
    offset: u64,
    value_len: u32,
}

impl ValuePointer {
    /// Bytes the pointer takes on disk.
    pub const ENCODED_LEN: usize = 16;

    const OFFSET_SHIFT: u32 = 32;
    const VALUE_LEN_SHIFT: u32 = 96;

    /// A pointer to a value of `value_len` bytes whose first record is at
    /// `offset` of the value-log file `file`.
    pub const fn new(file: FileId, offset: u64, value_len: u32) -> Result<Self, FormatError> {
        if offset < FIRST_RECORD_OFFSET || offset > MAX_VALUE_OFFSET {
            return Err(FormatError::ValueLog(ValueLogError::InvalidOffset {
                offset,
            }));
        }
        if value_len == 0 {
            return Err(FormatError::ValueLog(ValueLogError::EmptyValue));
        }
        Ok(Self {
            file,
            offset,
            value_len,
        })
    }

    /// The value-log file the value is in.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The offset of the value's first record in that file.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        self.value_len
    }

    /// The bytes to read at [`Self::offset`] to have every record of the
    /// value, when the file is of format `version`.
    pub fn extent_len(&self, version: FormatVersion) -> u64 {
        extent_len(self.value_len, version)
    }

    /// The offset just past the value's last record.
    pub fn end(&self, version: FormatVersion) -> u64 {
        self.offset + self.extent_len(version)
    }

    /// The sixteen bytes written to disk.
    pub const fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let fields = self.file.get() as u128
            | (self.offset as u128) << Self::OFFSET_SHIFT
            | (self.value_len as u128) << Self::VALUE_LEN_SHIFT;
        fields.to_le_bytes()
    }

    /// Reads a pointer. The checks run in a fixed order: file id, offset,
    /// value length.
    ///
    /// The place of a value that is not in the value log yet is not a
    /// pointer: its file id is the reserved one, and this gives
    /// [`FormatError::ReservedFileId`]. [`ValuePlace::decode`] reads both.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Result<Self, FormatError> {
        let fields = u128::from_le_bytes(bytes);
        let file = match FileId::decode((fields as u32).to_le_bytes()) {
            Ok(Some(file)) => file,
            Ok(None) => return Err(FormatError::MissingId),
            Err(error) => return Err(error),
        };
        Self::new(
            file,
            (fields >> Self::OFFSET_SHIFT) as u64,
            (fields >> Self::VALUE_LEN_SHIFT) as u32,
        )
    }

    /// Reads the value this pointer names from `extent`, the bytes of the
    /// file from [`Self::offset`] on, for a cell of the page `owner` that
    /// holds the pointer.
    ///
    /// `extent` must hold at least [`Self::extent_len`] bytes. Later bytes
    /// are ignored. The value must have the pointer's length and must name
    /// `owner` as its owner: a pointer that leads to a record of another
    /// value, or that was carried to another page without its value, is an
    /// error, not a value.
    pub fn read<'a>(
        &self,
        extent: &'a [u8],
        owner: PageId,
        version: FormatVersion,
    ) -> Result<Value<'a>, FormatError> {
        self.read_with(extent, owner, Layout::of(version))
    }

    /// The position in `leaf` of the entry that holds this pointer as its
    /// out-of-line value, if there is one.
    ///
    /// This is how the owner of a value is found in a flat leaf: the value's
    /// back pointer names the page, and the pointer names the entry.
    pub fn find_in_flat_leaf(&self, leaf: &FlatLeaf<'_>) -> Option<usize> {
        let pointer = self.encode();
        (0..leaf.len()).find(|&slot| leaf.value(slot) == Some(FlatValue::OutOfLine(&pointer)))
    }

    fn read_with<'a>(
        &self,
        extent: &'a [u8],
        owner: PageId,
        layout: Layout,
    ) -> Result<Value<'a>, FormatError> {
        let (first, rest) = ValueRecord::decode_with(extent, layout)?;
        if first.value_len != self.value_len {
            return Err(ValueLogError::WrongValueLength {
                expected: self.value_len,
                found: first.value_len,
            }
            .into());
        }
        if first.owner != owner {
            return Err(ValueLogError::WrongOwner {
                expected: owner,
                found: first.owner,
            }
            .into());
        }
        finish_value(extent, first, rest, layout).map(|(value, _)| value)
    }
}

/// A value that is not in the value log yet: what stands where its pointer
/// will be.
///
/// It has the shape of a [`ValuePointer`] that names no file. The file id
/// is [`FileId::RESERVED`], the eight bytes of the offset identify the
/// pending value, and the value length is the value's.
///
/// ```text
/// offset  size  field
///      0     4  the reserved file id, ff ff ff ff
///      4     8  the pending value's id, not all zero
///     12     4  value length
/// ```
///
/// The eight bytes are the log format's: they lead from the cell to the
/// value's bytes. The page formats copy and compare them and give them no
/// meaning, except that all zero is not an id.
///
/// A pending value may stand in a page in a buffer, in a log record and
/// inside a run of page deltas. It may not stand in a page version that a
/// segment makes durable (PD16 of `docs/lignindb/page-delta.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PendingValue {
    id: [u8; Self::ID_LEN],
    value_len: u32,
}

impl PendingValue {
    /// Bytes of the id of a pending value.
    pub const ID_LEN: usize = 8;

    /// The pending value `id`, of `value_len` bytes. An id that is all zero
    /// is [`FormatError::MissingId`], and a value of no bytes is never out
    /// of line.
    pub const fn new(id: [u8; Self::ID_LEN], value_len: u32) -> Result<Self, FormatError> {
        if u64::from_le_bytes(id) == 0 {
            return Err(FormatError::MissingId);
        }
        if value_len == 0 {
            return Err(FormatError::ValueLog(ValueLogError::EmptyValue));
        }
        Ok(Self { id, value_len })
    }

    /// The eight bytes that identify the value, as the log format defines
    /// them.
    pub const fn id(&self) -> [u8; Self::ID_LEN] {
        self.id
    }

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        self.value_len
    }
}

/// Where a value that is not in its cell is: in the value log, or not there
/// yet.
///
/// Sixteen bytes on disk either way. The file id tells the two apart:
/// [`FileId::RESERVED`] is a pending value, and every other id is a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValuePlace {
    /// The value is in the value log, at this pointer.
    Placed(ValuePointer),
    /// The value is not in the value log yet.
    Pending(PendingValue),
}

impl ValuePlace {
    /// Bytes the place takes on disk.
    pub const ENCODED_LEN: usize = ValuePointer::ENCODED_LEN;

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        match self {
            Self::Placed(pointer) => pointer.value_len(),
            Self::Pending(pending) => pending.value_len(),
        }
    }

    /// The pointer to the value, if the value is in the value log.
    pub const fn pointer(&self) -> Option<ValuePointer> {
        match *self {
            Self::Placed(pointer) => Some(pointer),
            Self::Pending(_) => None,
        }
    }

    /// The pending value, if the value is not in the value log yet.
    pub const fn pending(&self) -> Option<PendingValue> {
        match *self {
            Self::Placed(_) => None,
            Self::Pending(pending) => Some(pending),
        }
    }

    /// Whether the value is not in the value log yet.
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }

    /// The sixteen bytes written to disk.
    pub const fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        match self {
            Self::Placed(pointer) => pointer.encode(),
            Self::Pending(pending) => {
                let fields = FileId::RESERVED as u128
                    | (u64::from_le_bytes(pending.id) as u128) << ValuePointer::OFFSET_SHIFT
                    | (pending.value_len as u128) << ValuePointer::VALUE_LEN_SHIFT;
                fields.to_le_bytes()
            }
        }
    }

    /// Reads a place. The file id is looked at first: zero is
    /// [`FormatError::MissingId`], the reserved id is a pending value, whose
    /// id and then length are checked, and any other is a pointer, with the
    /// checks of [`ValuePointer::decode`].
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Result<Self, FormatError> {
        let fields = u128::from_le_bytes(bytes);
        if fields as u32 != FileId::RESERVED {
            return match ValuePointer::decode(bytes) {
                Ok(pointer) => Ok(Self::Placed(pointer)),
                Err(error) => Err(error),
            };
        }
        let id = ((fields >> ValuePointer::OFFSET_SHIFT) as u64).to_le_bytes();
        let value_len = (fields >> ValuePointer::VALUE_LEN_SHIFT) as u32;
        match PendingValue::new(id, value_len) {
            Ok(pending) => Ok(Self::Pending(pending)),
            Err(error) => Err(error),
        }
    }

    /// Whether the sixteen bytes `bytes` start with the reserved file id,
    /// which is what makes a place a pending value.
    pub(crate) fn names_no_file(bytes: &[u8]) -> bool {
        bytes.first_chunk() == Some(&FileId::RESERVED.to_le_bytes())
    }
}

/// The values of a value-log file, in file order.
///
/// Each item is a value and the pointer its owner must hold for the value to
/// be live. The first error ends the iteration. Returned by [`scan`].
#[derive(Debug, Clone)]
pub struct Values<'a> {
    header: FileHeader,
    file: FileId,
    layout: Layout,
    rest: &'a [u8],
    offset: u64,
}

impl Values<'_> {
    /// The file's header.
    pub const fn header(&self) -> FileHeader {
        self.header
    }

    /// The file's id.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The offset of the next value. After an error it stays at the value
    /// that failed, so it is the length of the file's valid prefix.
    pub const fn offset(&self) -> u64 {
        self.offset
    }
}

impl<'a> Iterator for Values<'a> {
    type Item = Result<(ValuePointer, Value<'a>), FormatError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let (value, rest) = match read_value_with(self.rest, self.layout) {
            Ok(read) => read,
            Err(error) => {
                self.rest = &[];
                return Some(Err(error));
            }
        };
        let pointer = ValuePointer {
            file: self.file,
            offset: self.offset,
            value_len: value.value_len,
        };
        self.offset += value.encoded_len() as u64;
        self.rest = rest;
        Some(Ok((pointer, value)))
    }
}

/// Reads a whole value-log file: checks its header and returns its values.
///
/// On [`FormatError::Truncated`] from the iterator, both counts start at the
/// first byte of the value that failed, which is at [`Values::offset`].
pub fn scan(file: &[u8]) -> Result<Values<'_>, FormatError> {
    let header = FileHeader::decode(file)?;
    scan_with(file, header, Layout::of(header.version()))
}

fn scan_with(file: &[u8], header: FileHeader, layout: Layout) -> Result<Values<'_>, FormatError> {
    let (FileKind::ValueLog, Some(id)) = (header.kind(), header.file_id()) else {
        return Err(ValueLogError::WrongFileKind {
            found: header.kind(),
        }
        .into());
    };
    Ok(Values {
        header,
        file: id,
        layout,
        rest: file.get(FileHeader::ENCODED_LEN..).unwrap_or_default(),
        offset: FIRST_RECORD_OFFSET,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::{
        checksum::MAX_CHECKSUMMED_LEN,
        flat_page::{FlatLeafBuilder, FlatPage},
        header::write_record,
        test_util::SplitMix64,
    };

    const V1: FormatVersion = FormatVersion::V1;

    fn page(raw: u32) -> PageId {
        PageId::new(raw).expect("non-zero")
    }

    fn random_owner(rng: &mut SplitMix64) -> PageId {
        page((rng.next_u64() as u32).max(1))
    }

    fn random_bytes(rng: &mut SplitMix64, min_len: usize, spread: u64) -> Vec<u8> {
        let len = min_len + (rng.next_u64() % spread) as usize;
        rng.bytes(len)
    }

    fn file_id(raw: u32) -> FileId {
        FileId::new(raw).expect("non-zero")
    }

    fn tiny(max_part_len: u32) -> Layout {
        Layout { max_part_len }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn value_log(error: ValueLogError) -> FormatError {
        FormatError::ValueLog(error)
    }

    fn raw_record(page: u32, part: u16, value_len: u32, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&page.to_le_bytes());
        payload.extend_from_slice(&part.to_le_bytes());
        payload.extend_from_slice(&value_len.to_le_bytes());
        payload.extend_from_slice(data);
        framed(&payload)
    }

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut record = Vec::new();
        write_record(payload, &mut record).expect("fits a frame");
        record
    }

    fn encoded(owner: PageId, value: &[u8]) -> Vec<u8> {
        encoded_with(owner, value, Layout::V1)
    }

    fn encoded_with(owner: PageId, value: &[u8], layout: Layout) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_value_with(owner, value, &mut bytes, layout).expect("encodes");
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<ValueRecord<'_>, FormatError> {
        ValueRecord::decode(bytes, V1).map(|(record, _)| record)
    }

    fn scanned(file: &[u8]) -> Result<usize, FormatError> {
        scan(file).map(Iterator::count)
    }

    fn file_header(kind: FileKind, sequence: u32) -> FileHeader {
        FileHeader::new(kind, V1, sequence).expect("valid")
    }

    fn records_of(bytes: &[u8], layout: Layout) -> Vec<&[u8]> {
        let mut records = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (record, after) = ValueRecord::decode_with(rest, layout).expect("decodes");
            records.push(&rest[..record.encoded_len()]);
            rest = after;
        }
        records
    }

    #[test]
    fn the_layout_constants_are_part_of_the_format() {
        assert_eq!(RECORD_HEADER_LEN, 18);
        assert_eq!(MAX_PART_LEN, (1 << 27) - 14);
        assert_eq!(MAX_VALUE_LEN, 4_294_967_295);
        assert_eq!(FIRST_RECORD_OFFSET, 20);
        assert_eq!(MAX_VALUE_OFFSET, (1 << 63) - 1);
        assert_eq!(ValuePointer::ENCODED_LEN, 16);

        let length_field = RecordHeader::ENCODED_LEN - Checksum::ENCODED_LEN;
        assert_eq!(
            length_field + FIELDS_LEN + MAX_PART_LEN,
            MAX_CHECKSUMMED_LEN,
            "the checksum of a full part covers as much as one checksum may"
        );

        assert_eq!(Layout::of(V1), Layout::V1);
        assert_eq!(Layout::V1.max_payload_len(), RecordHeader::MAX_PAYLOAD_LEN);
        assert_eq!(Layout::V1.max_value_len(), MAX_VALUE_LEN as u64);
        assert_eq!(Layout::V1.part_count(u32::MAX), 33);
    }

    #[test]
    fn a_record_matches_the_golden_bytes() {
        let bytes = encoded(page(7), b"lignin value");
        assert_eq!(
            hex(&bytes),
            "16000000cad6d7750700000000000c0000006c69676e696e2076616c7565"
        );
        let (record, rest) = ValueRecord::decode(&bytes, V1).expect("decodes");
        assert_eq!(record.owner(), page(7));
        assert_eq!(record.part(), 0);
        assert_eq!(record.value_len(), 12);
        assert_eq!(record.data(), b"lignin value");
        assert_eq!(record.encoded_len(), 30);
        assert!(rest.is_empty());

        let bytes = encoded(page(0x0102_0304), &[0x00, 0xFF]);
        assert_eq!(hex(&bytes), "0c00000027b6eda80403020100000200000000ff");
    }

    #[test]
    fn a_value_log_file_matches_the_golden_bytes() {
        let mut file = file_header(FileKind::ValueLog, 12).encode().to_vec();
        write_value(page(7), b"lignin value", &mut file).expect("encodes");
        write_value(page(0x0102_0304), &[0x00, 0xFF], &mut file).expect("encodes");
        assert_eq!(
            hex(&file),
            "4c49474e494e4442040001000c000000e6767d89\
             16000000cad6d7750700000000000c0000006c69676e696e2076616c7565\
             0c00000027b6eda80403020100000200000000ff"
        );

        let mut values = scan(&file).expect("a value log");
        assert_eq!(values.header(), file_header(FileKind::ValueLog, 12));
        assert_eq!(values.file(), file_id(12));
        assert_eq!(values.offset(), 20);

        let (pointer, value) = values.next().expect("a value").expect("valid");
        assert_eq!(
            pointer,
            ValuePointer::new(file_id(12), 20, 12).expect("valid")
        );
        assert_eq!(value.owner(), page(7));
        assert_eq!(value.contiguous(), Some(&b"lignin value"[..]));
        assert_eq!(values.offset(), 50);

        let (pointer, value) = values.next().expect("a value").expect("valid");
        assert_eq!(
            pointer,
            ValuePointer::new(file_id(12), 50, 2).expect("valid")
        );
        assert_eq!(value.owner(), page(0x0102_0304));
        assert_eq!(value.to_vec(), [0x00, 0xFF]);
        assert_eq!(values.offset(), file.len() as u64);

        assert!(values.next().is_none());
    }

    #[test]
    fn a_value_pointer_matches_the_golden_bytes() {
        let pointer = ValuePointer::new(file_id(12), 20, 12).expect("valid");
        assert_eq!(hex(&pointer.encode()), "0c00000014000000000000000c000000");

        let pointer = ValuePointer::new(file_id(0x0102_0304), 0x0506_0708_090A_0B0C, 0x0D0E_0F10)
            .expect("valid");
        assert_eq!(hex(&pointer.encode()), "040302010c0b0a0908070605100f0e0d");
        assert_eq!(ValuePointer::decode(pointer.encode()), Ok(pointer));
    }

    #[test]
    fn a_record_is_a_default_frame_around_its_fields_and_data() {
        let mut rng = SplitMix64::new(0xF4A3);
        for len in [1, 2, 13, 300] {
            let value = rng.bytes(len);
            let owner = random_owner(&mut rng);
            let bytes = encoded(owner, &value);
            assert_eq!(bytes, raw_record(owner.get(), 0, len as u32, &value));
            let (payload, rest) =
                read_record(&bytes, RecordHeader::MAX_PAYLOAD_LEN).expect("a framed record");
            assert_eq!(payload[..4], owner.encode());
            assert_eq!(payload[FIELDS_LEN..], value);
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn values_round_trip() {
        let mut rng = SplitMix64::new(0x7A1E);
        let trailing = [0xAB; 7];
        for len in (1..=300).chain([4095, 4096, 65_537]) {
            let value = rng.bytes(len);
            let owner = random_owner(&mut rng);
            let mut bytes = encoded(owner, &value);
            let encoded_len = bytes.len();
            assert_eq!(encoded_len, RECORD_HEADER_LEN + len);
            assert_eq!(encoded_len as u64, extent_len(len as u32, V1));
            assert_eq!(record_len(&bytes[..8], V1), Ok(encoded_len));
            bytes.extend_from_slice(&trailing);

            let (read, rest) = read_value(&bytes, V1).expect("reads");
            assert_eq!(rest, trailing);
            assert_eq!(read.owner(), owner);
            assert_eq!(read.value_len() as usize, len);
            assert_eq!(read.encoded_len(), encoded_len);
            assert_eq!(read.contiguous(), Some(&value[..]));
            assert_eq!(read.parts().collect::<Vec<_>>(), [&value[..]]);
            assert_eq!(read.to_vec(), value);
            assert_eq!(encoded(read.owner(), &read.to_vec()), bytes[..encoded_len]);

            let (record, rest) = ValueRecord::decode(&bytes, V1).expect("decodes");
            assert_eq!(rest, trailing);
            assert_eq!(record.data(), value);
        }
    }

    #[test]
    fn a_long_value_is_split_into_full_parts_and_a_last_part() {
        let mut rng = SplitMix64::new(0x5B11);
        for max_part_len in 1..=7 {
            let layout = tiny(max_part_len);
            let max = max_part_len as usize;
            for len in 1..=40 {
                let value = rng.bytes(len);
                let owner = random_owner(&mut rng);
                let bytes = encoded_with(owner, &value, layout);
                let count = len.div_ceil(max);
                assert_eq!(bytes.len(), len + RECORD_HEADER_LEN * count);
                assert_eq!(bytes.len() as u64, layout.extent_len(len as u32));

                let mut rest = &bytes[..];
                for part in 0..count {
                    let (record, after) = ValueRecord::decode_with(rest, layout).expect("decodes");
                    assert_eq!(record.owner(), owner);
                    assert_eq!(usize::from(record.part()), part);
                    assert_eq!(record.value_len() as usize, len);
                    assert_eq!(record.data(), &value[part * max..len.min((part + 1) * max)]);
                    assert!(record.data().len() == max || part == count - 1);
                    rest = after;
                }
                assert!(rest.is_empty());

                let (read, rest) = read_value_with(&bytes, layout).expect("reads");
                assert!(rest.is_empty());
                assert_eq!(read.owner(), owner);
                assert_eq!(read.encoded_len(), bytes.len());
                assert_eq!(read.parts().count(), count);
                assert_eq!(read.contiguous(), (count == 1).then_some(&value[..]));
                assert_eq!(read.to_vec(), value);
                assert_eq!(encoded_with(read.owner(), &read.to_vec(), layout), bytes);
            }
        }
    }

    #[test]
    fn a_value_above_what_one_checksum_covers_is_two_records() {
        let len = MAX_PART_LEN + 1;
        let block: Vec<u8> = (0..251).collect();
        let mut value = Vec::with_capacity(len);
        while value.len() < len {
            value.extend_from_slice(&block[..block.len().min(len - value.len())]);
        }
        let owner = page(9);

        let mut parts = encode_value(owner, &value).expect("fits");
        assert_eq!(parts.len(), 2);
        let (header, data) = parts.next().expect("part 0");
        assert_eq!(hex(&header), "fcffff07a5188493090000000000f3ffff07");
        assert_eq!(data.len(), MAX_PART_LEN);
        let (header, data) = parts.next().expect("part 1");
        assert_eq!(hex(&header), "0b0000003b79ef1a090000000100f3ffff07");
        assert_eq!(data, [value[MAX_PART_LEN]]);
        assert!(parts.next().is_none());

        let mut bytes = encoded(owner, &value);
        assert_eq!(bytes.len(), len + 2 * RECORD_HEADER_LEN);
        let pointer = ValuePointer::new(file_id(1), 20, len as u32).expect("valid");
        assert_eq!(pointer.extent_len(V1), bytes.len() as u64);
        assert_eq!(pointer.end(V1), 20 + bytes.len() as u64);

        let read = pointer.read(&bytes, owner, V1).expect("reads");
        assert_eq!(read.contiguous(), None);
        assert_eq!(
            read.parts().map(<[u8]>::len).collect::<Vec<_>>(),
            [MAX_PART_LEN, 1]
        );
        assert!(read.to_vec() == value);

        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(matches!(
            pointer.read(&bytes, owner, V1),
            Err(FormatError::ChecksumMismatch { .. })
        ));
        drop(bytes);

        value.truncate(MAX_PART_LEN);
        let bytes = encoded(owner, &value);
        assert_eq!(bytes.len(), MAX_PART_LEN + RECORD_HEADER_LEN);
        assert_eq!(record_len(&bytes[..8], V1), Ok(bytes.len()));
        let (read, rest) = read_value(&bytes, V1).expect("reads");
        assert!(rest.is_empty());
        assert!(read.contiguous() == Some(&value[..]));
    }

    #[test]
    fn value_lengths_are_checked_without_reading_the_value() {
        assert_eq!(
            checked_value_len(0),
            Err(value_log(ValueLogError::EmptyValue))
        );
        assert_eq!(checked_value_len(1), Ok(1));
        assert_eq!(checked_value_len(MAX_VALUE_LEN), Ok(u32::MAX));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            checked_value_len(MAX_VALUE_LEN + 1),
            Err(value_log(ValueLogError::ValueTooLong { len: 1 << 32 }))
        );

        assert_eq!(extent_len(1, V1), 19);
        assert_eq!(
            extent_len(MAX_PART_LEN as u32, V1),
            MAX_PART_LEN as u64 + 18
        );
        assert_eq!(
            extent_len(MAX_PART_LEN as u32 + 1, V1),
            MAX_PART_LEN as u64 + 37
        );
        assert_eq!(extent_len(u32::MAX, V1), u64::from(u32::MAX) + 33 * 18);

        let mut out = vec![1, 2, 3];
        assert_eq!(
            write_value(page(1), &[], &mut out),
            Err(value_log(ValueLogError::EmptyValue))
        );
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(
            encode_value(page(1), &[]).map(|parts| parts.len()),
            Err(value_log(ValueLogError::EmptyValue))
        );
    }

    #[test]
    fn a_value_has_at_most_as_many_parts_as_the_part_field_counts() {
        let layout = tiny(1);
        let owner = page(1);
        assert_eq!(layout.max_value_len(), 65_536);
        assert_eq!(layout.checked_value_len(65_536), Ok(65_536));
        assert_eq!(
            layout.checked_value_len(65_537),
            Err(ValueLogError::ValueTooLong { len: 65_537 })
        );
        assert_eq!(
            encode_value_with(owner, &[7; 65_537], layout).map(|parts| parts.len()),
            Err(value_log(ValueLogError::ValueTooLong { len: 65_537 }))
        );
        assert_eq!(
            ValueRecord::decode_with(&raw_record(1, 0, 65_537, &[7]), layout),
            Err(value_log(ValueLogError::NoSuchPart {
                part: 0,
                value_len: 65_537
            }))
        );
        assert_eq!(tiny(u32::MAX).part_len(u32::MAX, 1), None);

        let value: Vec<u8> = (0..65_536).map(|index| (index % 253) as u8).collect();
        let bytes = encoded_with(owner, &value, layout);
        let records = records_of(&bytes, layout);
        assert_eq!(records.len(), 65_536);
        let (last, _) = ValueRecord::decode_with(records[65_535], layout).expect("decodes");
        assert_eq!(last.part(), u16::MAX);
        let (read, rest) = read_value_with(&bytes, layout).expect("reads");
        assert!(rest.is_empty());
        assert_eq!(read.to_vec(), value);
    }

    #[test]
    fn every_shorter_input_is_rejected_as_truncated() {
        let bytes = encoded(page(7), b"lignin value");
        for available in 0..bytes.len() {
            let needed = if available < RecordHeader::ENCODED_LEN {
                RecordHeader::ENCODED_LEN
            } else {
                bytes.len()
            };
            let truncated = FormatError::Truncated { needed, available };
            let input = &bytes[..available];
            assert_eq!(decode(input), Err(truncated));
            assert_eq!(
                read_value(input, V1).map(|(value, _)| value),
                Err(truncated)
            );
            if available < RecordHeader::ENCODED_LEN {
                assert_eq!(record_len(input, V1), Err(truncated));
            } else {
                assert_eq!(record_len(input, V1), Ok(bytes.len()));
            }
        }

        let layout = tiny(5);
        let bytes = encoded_with(page(7), b"lignin value", layout);
        let record_lens = [23, 23, 20];
        assert_eq!(bytes.len(), record_lens.iter().sum::<usize>());
        for available in 0..bytes.len() {
            let mut start = 0;
            let mut record_len = 0;
            for len in record_lens {
                record_len = len;
                if available < start + len {
                    break;
                }
                start += len;
            }
            let needed = if available - start < RecordHeader::ENCODED_LEN {
                start + RecordHeader::ENCODED_LEN
            } else {
                start + record_len
            };
            assert_eq!(
                read_value_with(&bytes[..available], layout).map(|(value, _)| value),
                Err(FormatError::Truncated { needed, available }),
                "{available} bytes"
            );
        }
    }

    #[test]
    fn every_single_bit_flip_in_a_record_is_rejected() {
        let bytes = encoded(page(7), b"lignin value");
        let payload_len = (bytes.len() - RecordHeader::ENCODED_LEN) as u32;
        let (mut too_long, mut truncated, mut mismatched) = (0, 0, 0);
        for bit in 0..bytes.len() * 8 {
            let mut damaged = bytes.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            let error = ValueRecord::decode(&damaged, V1).expect_err("damaged");
            assert_eq!(read_value(&damaged, V1).map(|(value, _)| value), Err(error));
            match error {
                FormatError::RecordTooLong { .. } => {
                    assert!((27..32).contains(&bit));
                    too_long += 1;
                }
                FormatError::Truncated { .. } => {
                    assert!(bit < 27 && payload_len & (1 << bit) == 0);
                    truncated += 1;
                }
                FormatError::ChecksumMismatch { .. } => mismatched += 1,
                other => panic!("bit {bit}: {other}"),
            }
        }
        assert_eq!(too_long, 5);
        assert_eq!(truncated, 27 - payload_len.count_ones());
        assert_eq!(mismatched, bytes.len() as u32 * 8 - too_long - truncated);
    }

    #[test]
    fn a_record_with_a_valid_checksum_and_invalid_fields_is_rejected() {
        assert_eq!(
            decode(&raw_record(7, 0, 5, b"abcde")).map(|record| record.data().to_vec()),
            Ok(b"abcde".to_vec())
        );

        assert_eq!(
            decode(&raw_record(0, 0, 5, b"abcde")),
            Err(FormatError::MissingId)
        );
        assert_eq!(
            decode(&raw_record(7, 0, 0, b"abcde")),
            Err(value_log(ValueLogError::NoSuchPart {
                part: 0,
                value_len: 0
            }))
        );
        assert_eq!(
            decode(&raw_record(7, 1, 5, b"abcde")),
            Err(value_log(ValueLogError::NoSuchPart {
                part: 1,
                value_len: 5
            }))
        );
        for data in [&b""[..], b"abcd", b"abcdef"] {
            assert_eq!(
                decode(&raw_record(7, 0, 5, data)),
                Err(value_log(ValueLogError::WrongPartLength {
                    expected: 5,
                    found: data.len()
                }))
            );
        }
        for len in 0..FIELDS_LEN {
            assert_eq!(
                decode(&framed(&vec![1; len])),
                Err(value_log(ValueLogError::RecordTooShort { len }))
            );
        }
    }

    #[test]
    fn record_checks_run_in_the_specified_order() {
        assert_eq!(
            decode(&[0xFF; 7]),
            Err(FormatError::Truncated {
                needed: 8,
                available: 7
            })
        );

        let too_long = (RecordHeader::MAX_PAYLOAD_LEN as u32 + 1).to_le_bytes();
        let mut bytes = [0xFF; 8];
        bytes[..4].copy_from_slice(&too_long);
        let error = FormatError::RecordTooLong {
            len: RecordHeader::MAX_PAYLOAD_LEN as u64 + 1,
            max: RecordHeader::MAX_PAYLOAD_LEN as u64,
        };
        assert_eq!(decode(&bytes), Err(error));
        assert_eq!(record_len(&bytes, V1), Err(error));

        let mut bytes = raw_record(0, 9, 0, b"");
        bytes[4] ^= 1;
        assert!(matches!(
            decode(&bytes),
            Err(FormatError::ChecksumMismatch { .. })
        ));
        assert!(matches!(
            decode(&[0; 64]),
            Err(FormatError::ChecksumMismatch { .. })
        ));

        assert_eq!(
            decode(&raw_record(0, 9, 5, b"abcdef")),
            Err(FormatError::MissingId)
        );
        assert_eq!(
            decode(&raw_record(7, 9, 5, b"abcdef")),
            Err(value_log(ValueLogError::NoSuchPart {
                part: 9,
                value_len: 5
            }))
        );
    }

    #[test]
    fn a_record_decodes_only_from_the_bytes_it_encodes_to() {
        let mut rng = SplitMix64::new(0xDEC0);
        let layout = tiny(4);
        let (mut accepted, mut rejected) = (0, 0);
        for _ in 0..20_000 {
            let owner = (rng.next_u64() % 3) as u32;
            let part = (rng.next_u64() % 4) as u16;
            let value_len = (rng.next_u64() % 12) as u32;
            let data = random_bytes(&mut rng, 0, 6);
            let bytes = raw_record(owner, part, value_len, &data);

            let before = u32::from(part) * 4;
            let valid = owner != 0
                && value_len > before
                && data.len() as u32 == (value_len - before).min(4);
            match ValueRecord::decode_with(&bytes, layout) {
                Ok((record, rest)) => {
                    assert!(valid);
                    assert!(rest.is_empty());
                    let header =
                        record_header(record.owner, record.part, record.value_len, record.data);
                    assert_eq!([&header[..], record.data].concat(), bytes);
                    accepted += 1;
                }
                Err(_) => {
                    assert!(!valid);
                    rejected += 1;
                }
            }
        }
        assert!(accepted > 800 && rejected > 5_000);
    }

    #[test]
    fn a_value_is_read_only_from_its_own_parts_in_order() {
        let layout = tiny(4);
        let read = |records: &[&[u8]]| {
            let bytes = records.concat();
            read_value_with(&bytes, layout).map(|(value, _)| value.to_vec())
        };
        let bytes = encoded_with(page(7), b"0123456789", layout);
        let parts = records_of(&bytes, layout);
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            [22, 22, 20]
        );

        assert_eq!(read(&parts), Ok(b"0123456789".to_vec()));
        assert_eq!(
            read(&parts[1..]),
            Err(value_log(ValueLogError::UnexpectedPart {
                expected: 0,
                found: 1
            }))
        );
        assert_eq!(
            read(&[parts[0], parts[2], parts[1]]),
            Err(value_log(ValueLogError::UnexpectedPart {
                expected: 1,
                found: 2
            }))
        );
        assert_eq!(
            read(&[parts[0], parts[0]]),
            Err(value_log(ValueLogError::UnexpectedPart {
                expected: 1,
                found: 0
            }))
        );
        assert_eq!(
            read(&[parts[0], parts[1], parts[1]]),
            Err(value_log(ValueLogError::UnexpectedPart {
                expected: 2,
                found: 1
            }))
        );
        assert_eq!(
            read(&parts[..2]),
            Err(FormatError::Truncated {
                needed: 52,
                available: 44
            })
        );
        assert_eq!(
            read(&[parts[0], parts[1], &parts[2][..10]]),
            Err(FormatError::Truncated {
                needed: 64,
                available: 54
            })
        );

        let other_owner = encoded_with(page(8), b"0123456789", layout);
        let other_owner = records_of(&other_owner, layout);
        assert_eq!(
            read(&[parts[0], other_owner[1], parts[2]]),
            Err(value_log(ValueLogError::PartOfAnotherValue))
        );
        let longer = encoded_with(page(7), b"0123456789a", layout);
        let longer = records_of(&longer, layout);
        assert_eq!(
            read(&[parts[0], longer[1], parts[2]]),
            Err(value_log(ValueLogError::PartOfAnotherValue))
        );

        let mut damaged = parts[2].to_vec();
        damaged[19] ^= 0x40;
        assert!(matches!(
            read(&[parts[0], parts[1], &damaged]),
            Err(FormatError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn value_pointers_round_trip() {
        let mut rng = SplitMix64::new(0x9017);
        let offsets = [20, 21, 1 << 32, MAX_VALUE_OFFSET];
        let lens = [1, 2, MAX_PART_LEN as u32, MAX_PART_LEN as u32 + 1, u32::MAX];
        for round in 0..5_000 {
            let file = file_id((rng.next_u64() as u32).max(1));
            let offset = match offsets.get(round) {
                Some(&offset) => offset,
                None => (rng.next_u64() >> 1).max(20),
            };
            let value_len = match lens.get(round) {
                Some(&len) => len,
                None => (rng.next_u64() as u32).max(1),
            };
            let pointer = ValuePointer::new(file, offset, value_len).expect("valid");
            assert_eq!(pointer.file(), file);
            assert_eq!(pointer.offset(), offset);
            assert_eq!(pointer.value_len(), value_len);
            assert_eq!(pointer.extent_len(V1), extent_len(value_len, V1));
            assert_eq!(pointer.end(V1), offset + extent_len(value_len, V1));

            let bytes = pointer.encode();
            assert_eq!(bytes[..4], file.encode());
            assert_eq!(bytes[4..12], offset.to_le_bytes());
            assert_eq!(bytes[12..], value_len.to_le_bytes());
            assert_eq!(ValuePointer::decode(bytes), Ok(pointer));
        }
        let last = ValuePointer::new(file_id(1), MAX_VALUE_OFFSET, u32::MAX).expect("valid");
        assert_eq!(last.end(V1), (1 << 63) + (1 << 32) + 592);
    }

    #[test]
    fn a_value_pointer_decodes_only_from_the_bytes_it_encodes_to() {
        let mut rng = SplitMix64::new(0x9018);
        let (mut accepted, mut rejected) = (0, 0);
        for _ in 0..20_000 {
            let file = match rng.next_u64() % 4 {
                0 => 0,
                _ => rng.next_u64() as u32,
            };
            let offset = match rng.next_u64() % 8 {
                0 => 0,
                1 => 19,
                2 => 20,
                3 => MAX_VALUE_OFFSET,
                4 => MAX_VALUE_OFFSET + 1,
                5 => u64::MAX,
                _ => rng.next_u64(),
            };
            let value_len = match rng.next_u64() % 4 {
                0 => 0,
                _ => rng.next_u64() as u32,
            };
            let mut bytes = [0; ValuePointer::ENCODED_LEN];
            bytes[..4].copy_from_slice(&file.to_le_bytes());
            bytes[4..12].copy_from_slice(&offset.to_le_bytes());
            bytes[12..].copy_from_slice(&value_len.to_le_bytes());

            let expected = if file == 0 {
                Err(FormatError::MissingId)
            } else if !(20..=MAX_VALUE_OFFSET).contains(&offset) {
                Err(value_log(ValueLogError::InvalidOffset { offset }))
            } else if value_len == 0 {
                Err(value_log(ValueLogError::EmptyValue))
            } else {
                Ok(bytes)
            };
            let decoded = ValuePointer::decode(bytes);
            assert_eq!(decoded.map(|pointer| pointer.encode()), expected);
            if let (Ok(pointer), Some(file)) = (decoded, FileId::new(file)) {
                assert_eq!(ValuePointer::new(file, offset, value_len), Ok(pointer));
                accepted += 1;
            } else {
                rejected += 1;
            }
        }
        assert!(accepted > 1_000 && rejected > 1_000);
        assert_eq!(ValuePointer::decode([0; 16]), Err(FormatError::MissingId));
    }

    #[test]
    fn a_pending_value_is_a_place_that_names_no_file() {
        let waiting = PendingValue::new([1, 0, 0, 0, 0, 0, 0, 0], 12).expect("valid");
        let place = ValuePlace::Pending(waiting);
        assert_eq!(hex(&place.encode()), "ffffffff01000000000000000c000000");
        assert_eq!(ValuePlace::decode(place.encode()), Ok(place));

        let id = [0x0C, 0x0B, 0x0A, 0x09, 0x08, 0x07, 0x06, 0x05];
        let waiting = PendingValue::new(id, 0x0D0E_0F10).expect("valid");
        let place = ValuePlace::Pending(waiting);
        assert_eq!(hex(&place.encode()), "ffffffff0c0b0a0908070605100f0e0d");
        assert_eq!(ValuePlace::decode(place.encode()), Ok(place));
        assert_eq!((waiting.id(), waiting.value_len()), (id, 0x0D0E_0F10));
        assert_eq!(place.value_len(), 0x0D0E_0F10);
        assert_eq!((place.pending(), place.pointer()), (Some(waiting), None));
        assert!(place.is_pending());
        assert_eq!(PendingValue::ID_LEN, 8);
        assert_eq!(ValuePlace::ENCODED_LEN, 16);

        // A value pointer cannot hold it, so no function of the value log
        // is handed a place that names no file.
        assert_eq!(
            ValuePointer::decode(place.encode()),
            Err(FormatError::ReservedFileId)
        );
        assert_eq!(FileId::new(u32::MAX), None);

        // A place in a file is the pointer, byte for byte.
        let pointer = ValuePointer::new(file_id(12), 20, 12).expect("valid");
        let placed = ValuePlace::Placed(pointer);
        assert_eq!(placed.encode(), pointer.encode());
        assert_eq!(ValuePlace::decode(pointer.encode()), Ok(placed));
        assert_eq!((placed.pending(), placed.pointer()), (None, Some(pointer)));
        assert_eq!(placed.value_len(), 12);
        assert!(!placed.is_pending());
        let last = ValuePointer::new(FileId::MAX, 20, 12).expect("valid");
        assert_eq!(hex(&last.encode()), "feffffff14000000000000000c000000");
        assert_eq!(
            ValuePlace::decode(last.encode()),
            Ok(ValuePlace::Placed(last))
        );

        // The id is not all zero, and the value is not empty. The id is
        // looked at first.
        assert_eq!(PendingValue::new([0; 8], 12), Err(FormatError::MissingId));
        assert_eq!(PendingValue::new([0; 8], 0), Err(FormatError::MissingId));
        assert_eq!(
            PendingValue::new(id, 0),
            Err(value_log(ValueLogError::EmptyValue))
        );
        let raw = |id: u64, value_len: u32| {
            let mut bytes = [0xFF; 16];
            bytes[4..12].copy_from_slice(&id.to_le_bytes());
            bytes[12..].copy_from_slice(&value_len.to_le_bytes());
            bytes
        };
        assert_eq!(ValuePlace::decode(raw(0, 12)), Err(FormatError::MissingId));
        assert_eq!(ValuePlace::decode(raw(0, 0)), Err(FormatError::MissingId));
        assert_eq!(
            ValuePlace::decode(raw(1, 0)),
            Err(value_log(ValueLogError::EmptyValue))
        );
        assert_eq!(ValuePlace::decode([0; 16]), Err(FormatError::MissingId));
        // Every id with one bit set is an id, and an id is not an offset:
        // none of a pointer's rules about offsets apply to it.
        for bit in 0..64 {
            let bytes = raw(1 << bit, 1);
            let decoded = ValuePlace::decode(bytes).expect("a pending value");
            assert_eq!(
                decoded.pending().map(|p| p.id()),
                Some((1u64 << bit).to_le_bytes())
            );
            assert_eq!(decoded.encode(), bytes);
        }
        assert!(ValuePlace::decode(raw(u64::MAX, u32::MAX)).is_ok());
    }

    #[test]
    fn a_value_place_decodes_only_from_the_bytes_it_encodes_to() {
        let mut rng = SplitMix64::new(0x9019);
        let (mut placed, mut pending, mut rejected) = (0, 0, 0);
        for _ in 0..30_000 {
            let file = match rng.next_u64() % 6 {
                0 => 0,
                1 | 2 => u32::MAX,
                3 => u32::MAX - 1,
                _ => rng.next_u64() as u32,
            };
            let offset = match rng.next_u64() % 8 {
                0 => 0,
                1 => 19,
                2 => 20,
                3 => MAX_VALUE_OFFSET,
                4 => MAX_VALUE_OFFSET + 1,
                5 => u64::MAX,
                _ => rng.next_u64(),
            };
            let value_len = match rng.next_u64() % 4 {
                0 => 0,
                _ => rng.next_u64() as u32,
            };
            let mut bytes = [0; ValuePlace::ENCODED_LEN];
            bytes[..4].copy_from_slice(&file.to_le_bytes());
            bytes[4..12].copy_from_slice(&offset.to_le_bytes());
            bytes[12..].copy_from_slice(&value_len.to_le_bytes());

            let decoded = ValuePlace::decode(bytes);
            assert_eq!(ValuePlace::names_no_file(&bytes), file == u32::MAX);
            if file == u32::MAX {
                // The eight bytes are an id: any but zero.
                let expected = if offset == 0 {
                    Err(FormatError::MissingId)
                } else if value_len == 0 {
                    Err(value_log(ValueLogError::EmptyValue))
                } else {
                    Ok(bytes)
                };
                assert_eq!(decoded.map(|place| place.encode()), expected);
                assert_eq!(
                    ValuePointer::decode(bytes),
                    Err(FormatError::ReservedFileId)
                );
                if let Ok(place) = decoded {
                    let waiting = place.pending().expect("pending");
                    assert_eq!(waiting.id(), offset.to_le_bytes());
                    assert_eq!(waiting.value_len(), value_len);
                    assert_eq!(PendingValue::new(waiting.id(), value_len), Ok(waiting));
                    pending += 1;
                } else {
                    rejected += 1;
                }
            } else {
                // Any other file id: a pointer, with a pointer's checks.
                assert_eq!(decoded, ValuePointer::decode(bytes).map(ValuePlace::Placed));
                match decoded {
                    Ok(place) => {
                        assert_eq!(place.encode(), bytes);
                        placed += 1;
                    }
                    Err(_) => rejected += 1,
                }
            }
        }
        assert!(placed > 1_000 && pending > 1_000 && rejected > 1_000);
    }

    #[test]
    fn a_pointer_reads_only_the_value_it_names_for_the_page_that_holds_it() {
        let (first_owner, second_owner) = (page(7), page(8));
        let mut file = file_header(FileKind::ValueLog, 5).encode().to_vec();
        write_value(first_owner, b"first value", &mut file).expect("encodes");
        write_value(second_owner, b"second", &mut file).expect("encodes");
        let pointers: Vec<ValuePointer> = scan(&file)
            .expect("a value log")
            .map(|item| item.expect("valid").0)
            .collect();
        let [first, second] = pointers[..] else {
            panic!("two values");
        };
        let extent = |pointer: &ValuePointer| &file[pointer.offset() as usize..];

        let value = first.read(extent(&first), first_owner, V1).expect("reads");
        assert_eq!(value.to_vec(), b"first value");
        assert_eq!(value.encoded_len() as u64, first.extent_len(V1));
        let exact = &file[first.offset() as usize..first.end(V1) as usize];
        assert_eq!(first.read(exact, first_owner, V1), Ok(value));
        let value = second
            .read(extent(&second), second_owner, V1)
            .expect("reads");
        assert_eq!(value.to_vec(), b"second");

        assert_eq!(
            first.read(extent(&first), second_owner, V1),
            Err(value_log(ValueLogError::WrongOwner {
                expected: second_owner,
                found: first_owner
            }))
        );
        let stale =
            ValuePointer::new(first.file(), second.offset(), first.value_len()).expect("valid");
        assert_eq!(
            stale.read(extent(&stale), first_owner, V1),
            Err(value_log(ValueLogError::WrongValueLength {
                expected: 11,
                found: 6
            }))
        );
        assert_eq!(
            first.read(&exact[..exact.len() - 1], first_owner, V1),
            Err(FormatError::Truncated {
                needed: exact.len(),
                available: exact.len() - 1
            })
        );
        let misdirected =
            ValuePointer::new(first.file(), first.offset() + 1, first.value_len()).expect("valid");
        assert!(misdirected
            .read(extent(&misdirected), first_owner, V1)
            .is_err());

        let layout = tiny(4);
        let bytes = encoded_with(first_owner, b"0123456789", layout);
        let whole = ValuePointer::new(file_id(5), 20, 10).expect("valid");
        assert_eq!(
            whole
                .read_with(&bytes, first_owner, layout)
                .map(Value::to_vec),
            Ok(b"0123456789".to_vec())
        );
        assert_eq!(
            whole.read_with(&bytes[22..], first_owner, layout),
            Err(value_log(ValueLogError::UnexpectedPart {
                expected: 0,
                found: 1
            }))
        );
    }

    #[test]
    fn scan_yields_every_value_with_the_pointer_to_it() {
        let mut rng = SplitMix64::new(0x5CA7);
        for layout in [Layout::V1, tiny(16)] {
            let header = file_header(FileKind::ValueLog, 77);
            let mut file = header.encode().to_vec();
            let mut written = Vec::new();
            for _ in 0..200 {
                let value = random_bytes(&mut rng, 1, 60);
                let owner = random_owner(&mut rng);
                written.push((file.len() as u64, owner, value.clone()));
                write_value_with(owner, &value, &mut file, layout).expect("encodes");
            }

            let mut values = scan_with(&file, header, layout).expect("a value log");
            for (offset, owner, bytes) in &written {
                assert_eq!(values.offset(), *offset);
                let (pointer, value) = values.next().expect("a value").expect("valid");
                assert_eq!(pointer.file(), file_id(77));
                assert_eq!(pointer.offset(), *offset);
                assert_eq!(pointer.value_len() as usize, bytes.len());
                assert_eq!(value.owner(), *owner);
                assert_eq!(value.to_vec(), *bytes);
                assert_eq!(
                    pointer.read_with(&file[*offset as usize..], *owner, layout),
                    Ok(value)
                );
            }
            assert!(values.next().is_none());
            assert_eq!(values.offset(), file.len() as u64);
        }
    }

    #[test]
    fn scan_rejects_what_is_not_a_value_log() {
        for kind in FileKind::ALL {
            let mut file = file_header(kind, 9).encode().to_vec();
            write_value(page(1), b"value", &mut file).expect("encodes");
            if kind == FileKind::ValueLog {
                assert_eq!(scanned(&file), Ok(1));
            } else {
                assert_eq!(
                    scanned(&file),
                    Err(value_log(ValueLogError::WrongFileKind { found: kind }))
                );
            }
        }

        let header = file_header(FileKind::ValueLog, 9).encode();
        assert_eq!(scanned(&header), Ok(0));
        for available in [0, 19] {
            assert_eq!(
                scanned(&header[..available]),
                Err(FormatError::Truncated {
                    needed: 20,
                    available
                })
            );
        }
        let mut damaged = header;
        damaged[0] ^= 1;
        assert_eq!(scanned(&damaged), Err(FormatError::BadMagic));
        assert_eq!(scanned(&[0; 64]), Err(FormatError::BadMagic));
    }

    #[test]
    fn scan_stops_at_the_first_value_that_fails() {
        let mut file = file_header(FileKind::ValueLog, 9).encode().to_vec();
        let mut starts = Vec::new();
        for value in [&b"one"[..], b"second", b"the third"] {
            starts.push(file.len());
            write_value(page(4), value, &mut file).expect("encodes");
        }
        let lens = |file: &[u8]| -> Vec<Result<u32, FormatError>> {
            scan(file)
                .expect("a value log")
                .map(|item| item.map(|(_, value)| value.value_len()))
                .collect()
        };
        assert_eq!(lens(&file), [Ok(3), Ok(6), Ok(9)]);

        let torn = &file[..file.len() - 1];
        let truncated = FormatError::Truncated {
            needed: file.len() - starts[2],
            available: file.len() - starts[2] - 1,
        };
        assert_eq!(lens(torn), [Ok(3), Ok(6), Err(truncated)]);
        let mut values = scan(torn).expect("a value log");
        assert_eq!(values.by_ref().count(), 3);
        assert_eq!(values.offset(), starts[2] as u64);
        assert!(values.next().is_none());
        assert_eq!(values.offset(), starts[2] as u64);

        let mut damaged = file.clone();
        damaged[starts[1] + RECORD_HEADER_LEN] ^= 0x10;
        let results = lens(&damaged);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], Ok(3));
        assert!(matches!(
            results[1],
            Err(FormatError::ChecksumMismatch { .. })
        ));
        let mut values = scan(&damaged).expect("a value log");
        assert_eq!(values.by_ref().count(), 2);
        assert_eq!(values.offset(), starts[1] as u64);

        let mut zero_filled = file.clone();
        zero_filled.extend_from_slice(&[0; 100]);
        let results = lens(&zero_filled);
        assert_eq!(results[..3], [Ok(3), Ok(6), Ok(9)]);
        assert_eq!(results.len(), 4);
        assert!(matches!(
            results[3],
            Err(FormatError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn arbitrary_bytes_are_rejected_or_decode_to_what_encodes_back_to_them() {
        let mut rng = SplitMix64::new(0xF022);
        let mut rejected = 0;
        for _ in 0..20_000 {
            let mut bytes = random_bytes(&mut rng, 0, 64);
            if rng.next_u64().is_multiple_of(2) && bytes.len() >= 4 {
                let len = (rng.next_u64() % 40) as u32;
                bytes[..4].copy_from_slice(&len.to_le_bytes());
            }
            rejected += usize::from(ValueRecord::decode(&bytes, V1).is_err());
            rejected += usize::from(read_value(&bytes, V1).is_err());
            rejected += usize::from(scan(&bytes).is_err());
            let _ = record_len(&bytes, V1);
        }
        assert_eq!(rejected, 60_000);

        let layout = tiny(8);
        let header = file_header(FileKind::ValueLog, 3);
        let mut file = header.encode().to_vec();
        for _ in 0..12 {
            let value = random_bytes(&mut rng, 1, 30);
            write_value_with(random_owner(&mut rng), &value, &mut file, layout).expect("encodes");
        }
        let (mut intact, mut failed) = (0, 0);
        for _ in 0..20_000 {
            let mut damaged = file.clone();
            for _ in 0..1 + rng.next_u64() % 3 {
                let Some(body_len) = damaged.len().checked_sub(21) else {
                    break;
                };
                let at = 20 + (rng.next_u64() as usize) % (body_len + 1);
                match rng.next_u64() % 3 {
                    0 => damaged[at] = rng.next_u64() as u8,
                    1 => damaged[at] ^= 1 << (rng.next_u64() % 8),
                    _ => damaged.truncate(at),
                }
            }
            let mut values = scan_with(&damaged, header, layout).expect("a value log");
            let mut valid_len = 20;
            for item in values.by_ref() {
                let Ok((pointer, value)) = item else {
                    failed += 1;
                    break;
                };
                assert_eq!(pointer.offset(), valid_len);
                let start = valid_len as usize;
                valid_len += value.encoded_len() as u64;
                let end = valid_len as usize;
                assert_eq!(
                    encoded_with(value.owner(), &value.to_vec(), layout),
                    damaged[start..end]
                );
                assert_eq!(damaged[start..end], file[start..end]);
            }
            assert_eq!(values.offset(), valid_len);
            intact += usize::from(valid_len == damaged.len() as u64);
        }
        assert!(failed > 15_000 && intact > 100);
    }

    #[test]
    fn a_pointer_is_found_in_a_flat_leaf_only_as_an_out_of_line_value() {
        let pointer = ValuePointer::new(file_id(3), 20, 9).expect("valid");
        let other = ValuePointer::new(file_id(3), 47, 9).expect("valid");
        let leaf_of = |entries: &[(&[u8], FlatValue<'_>)]| {
            let mut builder = FlatLeafBuilder::new(PageId::MAX, b"k", 4096).expect("a builder");
            for (key, value) in entries {
                builder.push(key, *value).expect("fits");
            }
            builder.finish()
        };
        let find = |image: &[u8], pointer: ValuePointer| {
            let leaf = FlatPage::decode(image, V1)
                .expect("a flat page")
                .leaf()
                .expect("a leaf");
            pointer.find_in_flat_leaf(&leaf)
        };
        let (bytes, other_bytes) = (pointer.encode(), other.encode());

        let image = leaf_of(&[
            (b"k1", FlatValue::Inline(&bytes)),
            (b"k2", FlatValue::OutOfLine(&other_bytes)),
            (b"k3", FlatValue::Inline(b"inline value")),
            (b"k4", FlatValue::OutOfLine(&bytes)),
        ]);
        assert_eq!(find(&image, pointer), Some(3));
        assert_eq!(find(&image, other), Some(1));

        let image = leaf_of(&[
            (b"k0", FlatValue::Inline(b"a smaller key moves the slot")),
            (b"k1", FlatValue::Inline(&bytes)),
            (b"k2", FlatValue::OutOfLine(&other_bytes)),
        ]);
        assert_eq!(find(&image, pointer), None);
        assert_eq!(find(&image, other), Some(2));
        assert_eq!(find(&leaf_of(&[]), pointer), None);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Cell {
        key: u32,
        pointer: ValuePointer,
    }

    #[derive(Clone)]
    enum Page {
        Tree(Vec<Cell>),
        Flat(Vec<u8>),
    }

    type Pages = BTreeMap<PageId, Page>;

    fn cells_of(pages: &Pages, id: PageId) -> Vec<Cell> {
        match pages.get(&id) {
            None => Vec::new(),
            Some(Page::Tree(cells)) => cells.clone(),
            Some(Page::Flat(image)) => FlatPage::decode(image, V1)
                .expect("a flat page")
                .leaf()
                .expect("a leaf")
                .iter()
                .map(|(key, value)| {
                    let FlatValue::OutOfLine(pointer) = value else {
                        panic!("only value pointers");
                    };
                    let key: [u8; 4] = key.to_vec().try_into().expect("a 4-byte key");
                    let pointer: [u8; 16] = pointer.try_into().expect("a 16-byte pointer");
                    Cell {
                        key: u32::from_be_bytes(key),
                        pointer: ValuePointer::decode(pointer).expect("a valid pointer"),
                    }
                })
                .collect(),
        }
    }

    struct Snapshot {
        version: u64,
        pages: Pages,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Cleaned {
        moved: usize,
        dead: usize,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OwnersNotFound {
        still_live: usize,
    }

    struct Store {
        layout: Layout,
        files: BTreeMap<FileId, Vec<u8>>,
        live: BTreeMap<FileId, usize>,
        emptied: BTreeMap<FileId, u64>,
        current: FileId,
        pages: Pages,
        version: u64,
    }

    impl Store {
        fn new(layout: Layout) -> Self {
            Self {
                layout,
                files: BTreeMap::new(),
                live: BTreeMap::new(),
                emptied: BTreeMap::new(),
                current: FileId::MIN,
                pages: BTreeMap::new(),
                version: 0,
            }
        }

        fn is_flat(id: PageId) -> bool {
            id.get().is_multiple_of(2)
        }

        fn cells(&self, id: PageId) -> Vec<Cell> {
            cells_of(&self.pages, id)
        }

        fn snapshot(&self) -> Snapshot {
            Snapshot {
                version: self.version,
                pages: self.pages.clone(),
            }
        }

        fn set_cells(&mut self, id: PageId, mut cells: Vec<Cell>) {
            self.version += 1;
            if cells.is_empty() {
                self.pages.remove(&id);
            } else if Self::is_flat(id) {
                cells.sort_by_key(|cell| cell.key);
                let mut builder =
                    FlatLeafBuilder::new(id, b"", FlatPage::MAX_LEN).expect("a builder");
                for cell in cells {
                    let value = cell.pointer.encode();
                    builder
                        .push(&cell.key.to_be_bytes(), FlatValue::OutOfLine(&value))
                        .expect("fits");
                }
                self.pages.insert(id, Page::Flat(builder.finish()));
            } else {
                cells.rotate_left(1);
                self.pages.insert(id, Page::Tree(cells));
            }
        }

        fn find(&self, id: PageId, pointer: ValuePointer) -> Option<usize> {
            match self.pages.get(&id)? {
                Page::Tree(cells) => cells.iter().position(|cell| cell.pointer == pointer),
                Page::Flat(image) => {
                    let leaf = FlatPage::decode(image, V1).ok()?.leaf()?;
                    pointer.find_in_flat_leaf(&leaf)
                }
            }
        }

        fn rotate(&mut self) {
            self.current = file_id(self.current.get() + 1);
        }

        fn append(&mut self, owner: PageId, value: &[u8]) -> ValuePointer {
            let id = self.current;
            let file = self
                .files
                .entry(id)
                .or_insert_with(|| file_header(FileKind::ValueLog, id.get()).encode().to_vec());
            let offset = file.len() as u64;
            write_value_with(owner, value, file, self.layout).expect("encodes");
            *self.live.entry(id).or_default() += 1;
            ValuePointer::new(id, offset, value.len() as u32).expect("valid")
        }

        fn release(&mut self, pointer: ValuePointer) {
            *self.live.get_mut(&pointer.file()).expect("an account") -= 1;
        }

        fn stored(&self, pointer: ValuePointer) -> Vec<u8> {
            let file = &self.files[&pointer.file()];
            let (value, _) = read_value_with(&file[pointer.offset() as usize..], self.layout)
                .expect("a stored value");
            value.to_vec()
        }

        fn put(&mut self, id: PageId, key: u32, value: &[u8]) {
            let mut cells = self.cells(id);
            let pointer = self.append(id, value);
            match cells.iter_mut().find(|cell| cell.key == key) {
                Some(cell) => {
                    let old = std::mem::replace(&mut cell.pointer, pointer);
                    self.release(old);
                }
                None => cells.push(Cell { key, pointer }),
            }
            self.set_cells(id, cells);
        }

        fn take(&mut self, id: PageId, key: u32) -> Cell {
            let mut cells = self.cells(id);
            let at = cells
                .iter()
                .position(|cell| cell.key == key)
                .expect("the cell");
            let cell = cells.remove(at);
            self.set_cells(id, cells);
            cell
        }

        fn delete(&mut self, id: PageId, key: u32) {
            let cell = self.take(id, key);
            self.release(cell.pointer);
        }

        fn carry(&mut self, from: PageId, to: PageId, key: u32) {
            let cell = self.take(from, key);
            let mut cells = self.cells(to);
            cells.push(cell);
            self.set_cells(to, cells);
        }

        fn append_again(&mut self, id: PageId, key: u32) {
            let cell = self.take(id, key);
            let value = self.stored(cell.pointer);
            self.release(cell.pointer);
            self.put(id, key, &value);
        }

        fn move_cell(&mut self, from: PageId, to: PageId, key: u32) {
            self.carry(from, to, key);
            self.append_again(to, key);
        }

        fn read(&self, id: PageId, key: u32) -> Result<Vec<u8>, FormatError> {
            self.read_at(&self.pages, id, key)
        }

        fn read_at(&self, pages: &Pages, id: PageId, key: u32) -> Result<Vec<u8>, FormatError> {
            let cell = cells_of(pages, id)
                .into_iter()
                .find(|cell| cell.key == key)
                .expect("the cell");
            let file = &self.files[&cell.pointer.file()];
            cell.pointer
                .read_with(&file[cell.pointer.offset() as usize..], id, self.layout)
                .map(Value::to_vec)
        }

        fn delete_unpinned(&mut self, oldest_pin: Option<u64>) {
            let unreachable: Vec<FileId> = self
                .emptied
                .iter()
                .filter(|&(_, &emptied_at)| oldest_pin.is_none_or(|pin| pin >= emptied_at))
                .map(|(&id, _)| id)
                .collect();
            for id in unreachable {
                self.emptied.remove(&id);
                self.files.remove(&id);
            }
        }

        fn clean(&mut self, id: FileId) -> Result<Cleaned, OwnersNotFound> {
            assert_ne!(id, self.current);
            let file = self.files[&id].clone();
            let header = FileHeader::decode(&file).expect("a file header");
            let (mut moved, mut dead) = (0, 0);
            for item in scan_with(&file, header, self.layout).expect("a value log") {
                let (pointer, value) = item.expect("a valid value");
                let owner = value.owner();
                let Some(at) = self.find(owner, pointer) else {
                    dead += 1;
                    continue;
                };
                let mut cells = self.cells(owner);
                cells[at].pointer = self.append(owner, &value.to_vec());
                self.set_cells(owner, cells);
                self.release(pointer);
                moved += 1;
            }
            match self.live[&id] {
                0 => {
                    self.live.remove(&id);
                    self.emptied.insert(id, self.version);
                    Ok(Cleaned { moved, dead })
                }
                still_live => Err(OwnersNotFound { still_live }),
            }
        }
    }

    #[test]
    fn the_back_pointer_is_enough_to_find_the_owning_cell() {
        for (layout, seed) in [(tiny(16), 0xC1EA), (Layout::V1, 0xC1EB)] {
            let mut rng = SplitMix64::new(seed);
            let mut store = Store::new(layout);
            let mut values: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
            let mut home: BTreeMap<u32, PageId> = BTreeMap::new();
            let mut freed = BTreeSet::new();
            let (mut moved, mut dead, mut reused, mut multi_part) = (0, 0, 0, 0);
            let (mut from_tree, mut from_flat) = (0, 0);
            let mut created: u32 = 0;

            for step in 1..=6_000 {
                let id = page(1 + (rng.next_u64() % 24) as u32);
                let cells = store.cells(id);
                let picked =
                    (!cells.is_empty()).then(|| cells[(rng.next_u64() as usize) % cells.len()].key);
                let value = random_bytes(&mut rng, 1, 48);
                match (rng.next_u64() % 8, picked) {
                    (0..=1, _) | (2..=3, None) => {
                        created += 1;
                        let key = created.wrapping_mul(0x9E37_79B1);
                        reused += usize::from(cells.is_empty() && freed.contains(&id));
                        store.put(id, key, &value);
                        values.insert(key, value);
                        home.insert(key, id);
                    }
                    (2..=3, Some(key)) => {
                        store.put(id, key, &value);
                        values.insert(key, value);
                    }
                    (4..=5, Some(key)) => {
                        store.delete(id, key);
                        values.remove(&key);
                        home.remove(&key);
                    }
                    (6, Some(key)) => {
                        let to = page(1 + (rng.next_u64() % 24) as u32);
                        if to != id {
                            store.move_cell(id, to, key);
                            home.insert(key, to);
                        }
                    }
                    _ => {}
                }
                if store.cells(id).is_empty() {
                    freed.insert(id);
                }

                if step % 500 != 0 {
                    continue;
                }
                store.rotate();
                let oldest = *store.files.keys().next().expect("a file");
                let owners: Vec<(PageId, Cell)> = home
                    .values()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .flat_map(|&id| store.cells(id).into_iter().map(move |cell| (id, cell)))
                    .filter(|(_, cell)| cell.pointer.file() == oldest)
                    .collect();
                let file = &store.files[&oldest];
                let header = FileHeader::decode(file).expect("a file header");
                let scanned = scan_with(file, header, layout)
                    .expect("a value log")
                    .count();
                multi_part += owners
                    .iter()
                    .filter(|(_, cell)| layout.part_count(cell.pointer.value_len()) > 1)
                    .count();
                from_flat += owners.iter().filter(|(id, _)| Store::is_flat(*id)).count();
                from_tree += owners.iter().filter(|(id, _)| !Store::is_flat(*id)).count();

                let cleaned = store.clean(oldest).expect("every owner was found");
                store.delete_unpinned(None);

                assert_eq!(cleaned.moved, owners.len());
                assert_eq!(cleaned.moved + cleaned.dead, scanned);
                assert!(!store.files.contains_key(&oldest));
                for (&key, &id) in &home {
                    assert_eq!(store.read(id, key).as_ref(), Ok(&values[&key]));
                    let cells = store.cells(id);
                    let cell = cells.iter().find(|cell| cell.key == key).expect("the cell");
                    assert_ne!(cell.pointer.file(), oldest);
                }
                moved += cleaned.moved;
                dead += cleaned.dead;
            }

            assert!(moved > 300 && dead > 1_000 && reused > 20);
            assert!(from_tree > 100 && from_flat > 100);
            assert_eq!(multi_part > 100, layout != Layout::V1);
        }
    }

    #[test]
    fn a_cell_carried_to_another_page_without_its_value_is_never_dropped() {
        for (from, to) in [(1, 3), (2, 4), (1, 2), (2, 1)] {
            let (from, to) = (page(from), page(to));
            let mut store = Store::new(Layout::V1);
            store.put(from, 10, b"carried without its value");
            store.put(from, 20, b"stays where it is");
            store.put(from, 30, b"soon overwritten");
            store.put(from, 30, b"overwritten");
            let first = file_id(1);
            store.rotate();

            store.carry(from, to, 10);
            let stale = store.cells(to)[0].pointer;
            assert_eq!(stale.file(), first);

            assert_eq!(store.find(from, stale), None);
            assert!(store.find(to, stale).is_some());
            assert_eq!(
                store.read(to, 10),
                Err(value_log(ValueLogError::WrongOwner {
                    expected: to,
                    found: from
                }))
            );

            assert_eq!(store.clean(first), Err(OwnersNotFound { still_live: 1 }));
            store.delete_unpinned(None);
            assert!(store.files.contains_key(&first));
            assert_eq!(store.stored(stale), b"carried without its value");
            assert_eq!(store.cells(to)[0].pointer, stale);
            assert_eq!(store.read(from, 20), Ok(b"stays where it is".to_vec()));
            assert_eq!(store.read(from, 30), Ok(b"overwritten".to_vec()));

            store.append_again(to, 10);
            assert_eq!(
                store.read(to, 10),
                Ok(b"carried without its value".to_vec())
            );
            assert_eq!(store.clean(first), Ok(Cleaned { moved: 0, dead: 4 }));
            store.delete_unpinned(None);
            assert!(!store.files.contains_key(&first));
            assert_eq!(
                store.read(to, 10),
                Ok(b"carried without its value".to_vec())
            );
            assert_eq!(store.read(from, 20), Ok(b"stays where it is".to_vec()));
        }
    }

    #[test]
    fn a_cleaned_file_outlives_the_snapshots_that_can_reach_it() {
        for (from, to) in [(1, 3), (2, 4), (1, 2), (2, 1)] {
            let (from, to) = (page(from), page(to));
            let before = [
                (10, &b"moved to another page later"[..]),
                (20, b"overwritten later"),
                (30, b"deleted later"),
                (40, b"only moved by the cleaner"),
            ];
            let mut store = Store::new(Layout::V1);
            for (key, value) in before {
                store.put(from, key, value);
            }
            let first = file_id(1);
            store.rotate();
            let snapshot = store.snapshot();

            store.move_cell(from, to, 10);
            store.put(from, 20, b"its new value");
            store.delete(from, 30);
            assert_eq!(store.clean(first), Ok(Cleaned { moved: 1, dead: 3 }));

            store.delete_unpinned(Some(snapshot.version));
            assert!(store.files.contains_key(&first));
            for (key, value) in before {
                assert_eq!(
                    store.read_at(&snapshot.pages, from, key),
                    Ok(value.to_vec())
                );
            }

            let later = store.snapshot();
            store.delete_unpinned(Some(later.version));
            assert!(!store.files.contains_key(&first));
            assert_eq!(
                store.read_at(&later.pages, to, 10),
                Ok(before[0].1.to_vec())
            );
            assert_eq!(store.read(from, 20), Ok(b"its new value".to_vec()));
            assert_eq!(store.read(from, 40), Ok(before[3].1.to_vec()));
        }
    }

    #[test]
    fn errors_say_what_is_wrong() {
        let messages = [
            (
                ValueLogError::WrongFileKind {
                    found: FileKind::Segment,
                },
                "a Segment file is not a value log",
            ),
            (
                ValueLogError::RecordTooShort { len: 9 },
                "record payload of 9 bytes is shorter than its 10 bytes of fields",
            ),
            (
                ValueLogError::NoSuchPart {
                    part: 2,
                    value_len: 5,
                },
                "a value of 5 bytes has no part 2",
            ),
            (
                ValueLogError::WrongPartLength {
                    expected: 5,
                    found: 4,
                },
                "part carries 4 bytes, expected 5",
            ),
            (
                ValueLogError::UnexpectedPart {
                    expected: 1,
                    found: 2,
                },
                "found part 2 where part 1 belongs",
            ),
            (
                ValueLogError::PartOfAnotherValue,
                "part names another owner or value length than part 0",
            ),
            (
                ValueLogError::WrongValueLength {
                    expected: 11,
                    found: 6,
                },
                "pointer states a value of 11 bytes, the record one of 6",
            ),
            (
                ValueLogError::WrongOwner {
                    expected: page(7),
                    found: page(8),
                },
                "value is owned by page 8, not by page 7",
            ),
            (ValueLogError::EmptyValue, "an empty value has no record"),
            (
                ValueLogError::ValueTooLong { len: 1 << 32 },
                "value of 4294967296 bytes is longer than the value log stores",
            ),
            (
                ValueLogError::InvalidOffset { offset: 19 },
                "no value starts at offset 19",
            ),
        ];
        for (error, message) in messages {
            assert_eq!(error.to_string(), message);
            let wrapped = FormatError::from(error);
            assert_eq!(wrapped, FormatError::ValueLog(error));
            assert_eq!(wrapped.to_string(), format!("value log: {message}"));
            let source: &dyn std::error::Error = &error;
            assert!(source.source().is_none());
        }
    }
}
