//! The values a commit group carries: the bytes of a new value, in one
//! record or several, or the name of a carried one.

use std::iter::FusedIterator;

use super::{
    place::LogAddress,
    record::{rebase, Fields, Record, CARRIED, VALUE},
    Limits, WalError, CARRIED_RECORD_LEN,
};
use crate::{
    checksum::Checksum,
    error::FormatError,
    header::{write_record, FormatVersion, RecordHeader},
    ids::PageId,
    value_log::{checked_value_len, PendingValue, ValueLogError, ValuePointer},
};

pub(super) const VALUE_FIELDS_LEN: usize = 1 + PageId::ENCODED_LEN + 2 + 4;
pub(super) const CARRIED_PAYLOAD_LEN: usize =
    1 + 2 * PageId::ENCODED_LEN + ValuePointer::ENCODED_LEN;

const fn limits_of(version: FormatVersion) -> Limits {
    match version {
        FormatVersion::V1 => Limits::FORMAT,
    }
}

const fn part_count(value_len: u32, limits: Limits) -> u64 {
    (value_len as u64).div_ceil(limits.max_value_part() as u64)
}

const fn part_len(value_len: u32, part: u16, limits: Limits) -> usize {
    let before = part as u64 * limits.max_value_part() as u64;
    let left = (value_len as u64).saturating_sub(before);
    if left < limits.max_value_part() as u64 {
        left as usize
    } else {
        limits.max_value_part()
    }
}

const fn extent_len(value_len: u32, limits: Limits) -> u64 {
    value_len as u64
        + part_count(value_len, limits) * (RecordHeader::ENCODED_LEN + VALUE_FIELDS_LEN) as u64
}

/// The bytes the records of a value of `value_len` bytes take in a log file
/// of format `version`: the value and one record header for each part.
pub const fn logged_extent_len(value_len: u32, version: FormatVersion) -> u64 {
    extent_len(value_len, limits_of(version))
}

/// The bytes to read at the address of a pending value of `value_len`
/// bytes to have all of it, whichever record stands there: the records of
/// the value, or the one record of a carried value.
pub const fn logged_read_len(value_len: u32, version: FormatVersion) -> u64 {
    let extent = logged_extent_len(value_len, version);
    if extent < CARRIED_RECORD_LEN as u64 {
        CARRIED_RECORD_LEN as u64
    } else {
        extent
    }
}

/// One record of a value in the log: a part of the value's bytes, with the
/// page whose cell the value belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValuePart<'a> {
    owner: PageId,
    part: u16,
    value_len: u32,
    data: &'a [u8],
    last: bool,
}

impl<'a> ValuePart<'a> {
    /// The page whose cell the value belongs to.
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

    /// Whether this is the last record of its value.
    pub const fn is_last(&self) -> bool {
        self.last
    }

    pub(super) fn decode(
        mut fields: Fields<'a>,
        version: FormatVersion,
        limits: Limits,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        let len = fields.rest().len() + 1;
        if len < VALUE_FIELDS_LEN {
            return Err(WalError::RecordLength { kind: VALUE, len }.into());
        }
        let owner = fields.page()?;
        let part = fields.u16()?;
        let value_len = fields.u32()?;
        let data = fields.take_rest();
        if value_len == 0 {
            return Err(ValueLogError::EmptyValue.into());
        }
        let parts = part_count(value_len, limits);
        if u64::from(part) >= parts {
            return Err(WalError::NoSuchValuePart { part, value_len }.into());
        }
        let expected = part_len(value_len, part, limits);
        if data.len() != expected {
            return Err(WalError::WrongValuePartLength {
                expected,
                found: data.len(),
            }
            .into());
        }
        Ok(Self {
            owner,
            part,
            value_len,
            data,
            last: u64::from(part) + 1 == parts,
        })
    }

    /// Checks that `self` is part number `expected` of the value of
    /// `value_len` bytes that belongs to a cell of the page `owner`.
    pub(super) fn check_continues(
        &self,
        owner: PageId,
        value_len: u32,
        expected: u16,
    ) -> Result<(), FormatError> {
        if self.owner != owner || self.value_len != value_len {
            return Err(WalError::PartOfAnotherValue.into());
        }
        if self.part != expected {
            return Err(WalError::UnexpectedValuePart {
                expected,
                found: self.part,
            }
            .into());
        }
        Ok(())
    }
}

/// Appends the records of `value`, for a cell of the page `owner`, to
/// `out`, and returns the value's length.
pub(super) fn write_value(
    owner: PageId,
    value: &[u8],
    limits: Limits,
    out: &mut Vec<u8>,
) -> Result<u32, FormatError> {
    let value_len = checked_value_len(value.len())?;
    let too_long = ValueLogError::ValueTooLong {
        len: value.len() as u64,
    };
    if part_count(value_len, limits) > u64::from(u16::MAX) + 1 {
        return Err(too_long.into());
    }
    out.reserve(extent_len(value_len, limits) as usize);
    for (part, data) in value.chunks(limits.max_value_part()).enumerate() {
        let mut fields = [0; VALUE_FIELDS_LEN];
        fields[0] = VALUE;
        fields[1..5].copy_from_slice(&owner.encode());
        fields[5..7].copy_from_slice(&(part as u16).to_le_bytes());
        fields[7..].copy_from_slice(&value_len.to_le_bytes());
        let payload_len = ((VALUE_FIELDS_LEN + data.len()) as u32).to_le_bytes();
        let checksum = Checksum::of(&[&payload_len, &fields, data]);
        out.extend_from_slice(&payload_len);
        out.extend_from_slice(&checksum.encode());
        out.extend_from_slice(&fields);
        out.extend_from_slice(data);
    }
    Ok(value_len)
}

/// A carried value: a value that its commit group does not repeat. The
/// record names the value where it lies in the value log, as the cell held
/// it before the commit carried the cell to another page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CarriedValue {
    owner: PageId,
    source_owner: PageId,
    source: ValuePointer,
}

impl CarriedValue {
    /// The value of a cell that was carried to the page `owner` and that
    /// lies at `source` in the value log, in a record whose owner is the
    /// page `source_owner`.
    pub const fn new(owner: PageId, source_owner: PageId, source: ValuePointer) -> Self {
        Self {
            owner,
            source_owner,
            source,
        }
    }

    /// The page whose cell the value belongs to now.
    pub const fn owner(&self) -> PageId {
        self.owner
    }

    /// The page that the value's record in the value log names as its
    /// owner: what a read through [`Self::source`] is checked against.
    pub const fn source_owner(&self) -> PageId {
        self.source_owner
    }

    /// Where the value's bytes are in the value log.
    pub const fn source(&self) -> ValuePointer {
        self.source
    }

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        self.source.value_len()
    }

    pub(super) fn write(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let mut payload = [0; CARRIED_PAYLOAD_LEN];
        payload[0] = CARRIED;
        payload[1..5].copy_from_slice(&self.owner.encode());
        payload[5..9].copy_from_slice(&self.source_owner.encode());
        payload[9..].copy_from_slice(&self.source.encode());
        write_record(&payload, out)
    }

    pub(super) fn decode(
        mut fields: Fields<'_>,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        fields.expect_len(CARRIED, CARRIED_PAYLOAD_LEN - 1)?;
        Ok(Self {
            owner: fields.page()?,
            source_owner: fields.page()?,
            source: fields.pointer()?,
        })
    }
}

/// The bytes of a value in the log: its records, one after another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoggedBytes<'a> {
    owner: PageId,
    value_len: u32,
    records: &'a [u8],
}

impl<'a> LoggedBytes<'a> {
    /// The page whose cell the value belongs to.
    pub const fn owner(&self) -> PageId {
        self.owner
    }

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        self.value_len
    }

    /// The value's bytes, one slice for each record, in order.
    pub const fn parts(&self) -> LoggedParts<'a> {
        LoggedParts { rest: self.records }
    }

    /// The value's bytes, when one record holds all of them.
    pub fn contiguous(&self) -> Option<&'a [u8]> {
        let mut parts = self.parts();
        let first = parts.next()?;
        parts.next().is_none().then_some(first)
    }

    /// The value's bytes in one allocation.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut value = Vec::with_capacity(self.value_len as usize);
        self.parts().for_each(|part| value.extend_from_slice(part));
        value
    }
}

/// The bytes of a value in the log, record by record.
#[derive(Debug, Clone)]
pub struct LoggedParts<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for LoggedParts<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let frame: &[u8; 4] = self.rest.first_chunk()?;
        let len = RecordHeader::ENCODED_LEN + u32::from_le_bytes(*frame) as usize;
        let (record, rest) = self.rest.split_at_checked(len)?;
        self.rest = rest;
        record.get(RecordHeader::ENCODED_LEN + VALUE_FIELDS_LEN..)
    }
}

impl FusedIterator for LoggedParts<'_> {}

/// What stands at the address of a pending value: the value's bytes, or
/// the name of the record in the value log that holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggedValue<'a> {
    /// The value's bytes.
    Bytes(LoggedBytes<'a>),
    /// The value was carried, and lies in the value log.
    Carried(CarriedValue),
}

impl<'a> LoggedValue<'a> {
    /// The page whose cell the value belongs to.
    pub const fn owner(&self) -> PageId {
        match self {
            Self::Bytes(bytes) => bytes.owner(),
            Self::Carried(carried) => carried.owner(),
        }
    }

    /// The value's length in bytes.
    pub const fn value_len(&self) -> u32 {
        match self {
            Self::Bytes(bytes) => bytes.value_len(),
            Self::Carried(carried) => carried.value_len(),
        }
    }

    /// The value's bytes, if the log holds them.
    pub const fn bytes(&self) -> Option<LoggedBytes<'a>> {
        match *self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Carried(_) => None,
        }
    }

    /// The name of the value in the value log, if the log does not hold
    /// its bytes.
    pub const fn carried(&self) -> Option<CarriedValue> {
        match *self {
            Self::Bytes(_) => None,
            Self::Carried(carried) => Some(carried),
        }
    }
}

/// Reads the value whose first record is at the start of `bytes`, in a log
/// file of format `version`, and returns it with the length of its records.
/// Later bytes are ignored.
///
/// Each record is verified. A value in several records must have all of
/// them, in order and one after another.
pub fn read_logged_value(
    bytes: &[u8],
    version: FormatVersion,
) -> Result<(LoggedValue<'_>, usize), FormatError> {
    read_with(bytes, version, limits_of(version))
}

pub(super) fn read_with(
    bytes: &[u8],
    version: FormatVersion,
    limits: Limits,
) -> Result<(LoggedValue<'_>, usize), FormatError> {
    let (record, mut len) = Record::decode_with(bytes, version, limits)?;
    let first = match record {
        Record::Value(first) => first,
        Record::Carried(carried) => return Ok((LoggedValue::Carried(carried), len)),
        other => {
            return Err(WalError::NotAValue {
                kind: other.kind_code(),
            }
            .into())
        }
    };
    first.check_continues(first.owner(), first.value_len(), 0)?;
    let mut last = first.is_last();
    let mut expected = 1;
    while !last {
        let rest = bytes.get(len..).unwrap_or_default();
        let (record, record_len) =
            Record::decode_with(rest, version, limits).map_err(|error| rebase(error, len))?;
        let Record::Value(part) = record else {
            return Err(WalError::UnfinishedValue.into());
        };
        part.check_continues(first.owner(), first.value_len(), expected)?;
        last = part.is_last();
        expected += 1;
        len += record_len;
    }
    let value = LoggedBytes {
        owner: first.owner(),
        value_len: first.value_len(),
        records: bytes.get(..len).unwrap_or_default(),
    };
    Ok((LoggedValue::Bytes(value), len))
}

/// Reads the pending value `pending` for the cell of the page `owner` that
/// holds it. `bytes` are the bytes of the log from the address
/// [`LogAddress::of`] gives, [`logged_read_len`] of them or up to the end
/// of the file.
///
/// What stands there must be of the pending value's length and must name
/// `owner`: an address that leads to the record of another value is an
/// error, not a value.
pub fn read_pending_value<'a>(
    pending: &PendingValue,
    bytes: &'a [u8],
    owner: PageId,
    version: FormatVersion,
) -> Result<LoggedValue<'a>, FormatError> {
    LogAddress::of(pending)?;
    let (value, _) = read_logged_value(bytes, version)?;
    if value.value_len() != pending.value_len() {
        return Err(WalError::WrongValueLength {
            expected: pending.value_len(),
            found: value.value_len(),
        }
        .into());
    }
    if value.owner() != owner {
        return Err(WalError::WrongOwner {
            expected: owner,
            found: value.owner(),
        }
        .into());
    }
    Ok(value)
}
