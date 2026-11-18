//! One record of a log file: its frame, its kind and its fields.

use super::{
    change::Change,
    group::GroupPart,
    place::LogStart,
    value::{CarriedValue, ValuePart},
    Limits, WalError,
};
use crate::{
    error::FormatError,
    header::{write_record, FormatVersion, RecordHeader},
    ids::{CommitVersion, PageId},
    value_log::{ValuePlace, ValuePointer},
};

pub(super) const START: u8 = 1;
pub(super) const VALUE: u8 = 2;
pub(super) const CARRIED: u8 = 3;
pub(super) const PART: u8 = 4;
pub(super) const CHANGE: u8 = 5;
pub(super) const MARK: u8 = 6;

pub(super) const START_PAYLOAD_LEN: usize = 1 + 8 + CommitVersion::ENCODED_LEN;
pub(super) const CHECKPOINT_PAYLOAD_LEN: usize = 1 + CommitVersion::ENCODED_LEN;

/// Adds `by` to both counts of a [`FormatError::Truncated`], for an error
/// that was counted from a byte `by` bytes into what the caller was given.
pub(super) fn rebase(error: FormatError, by: usize) -> FormatError {
    match error {
        FormatError::Truncated { needed, available } => FormatError::Truncated {
            needed: needed.saturating_add(by),
            available: available.saturating_add(by),
        },
        other => other,
    }
}

/// The fields of a record, read one after another. A field that is not
/// there is [`FormatError::Truncated`], counted from the first byte of the
/// record.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
    base: usize,
}

impl<'a> Fields<'a> {
    /// The fields in `bytes`, which start `base` bytes into their record.
    pub(super) const fn new(bytes: &'a [u8], base: usize) -> Self {
        Self { bytes, at: 0, base }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    pub(super) const fn position(&self) -> usize {
        self.at
    }

    pub(super) fn rest(&self) -> &'a [u8] {
        self.bytes.get(self.at..).unwrap_or_default()
    }

    fn truncated(&self, len: usize) -> FormatError {
        FormatError::Truncated {
            needed: self.base.saturating_add(self.at).saturating_add(len),
            available: self.base.saturating_add(self.bytes.len()),
        }
    }

    pub(super) fn take(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let part = self.rest().get(..len).ok_or_else(|| self.truncated(len))?;
        self.at += len;
        Ok(part)
    }

    pub(super) fn take_rest(&mut self) -> &'a [u8] {
        let rest = self.rest();
        self.at = self.bytes.len();
        rest
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<&'a [u8; N], FormatError> {
        let array = self.rest().first_chunk().ok_or_else(|| self.truncated(N))?;
        self.at += N;
        Ok(array)
    }

    pub(super) fn byte(&mut self) -> Result<u8, FormatError> {
        self.array().map(|&[byte]| byte)
    }

    pub(super) fn u16(&mut self) -> Result<u16, FormatError> {
        self.array().map(|bytes| u16::from_le_bytes(*bytes))
    }

    pub(super) fn u32(&mut self) -> Result<u32, FormatError> {
        self.array().map(|bytes| u32::from_le_bytes(*bytes))
    }

    pub(super) fn page(&mut self) -> Result<PageId, FormatError> {
        PageId::decode(*self.array()?).ok_or(FormatError::MissingId)
    }

    pub(super) fn pointer(&mut self) -> Result<ValuePointer, FormatError> {
        ValuePointer::decode(*self.array()?)
    }

    pub(super) fn place(&mut self) -> Result<ValuePlace, FormatError> {
        ValuePlace::decode(*self.array()?)
    }

    /// Checks that exactly `len` bytes are left, for a record of kind
    /// `kind` whose fields have one length.
    pub(super) fn expect_len(&self, kind: u8, len: usize) -> Result<(), FormatError> {
        if self.rest().len() != len {
            return Err(WalError::RecordLength {
                kind,
                len: self.bytes.len(),
            }
            .into());
        }
        Ok(())
    }
}

/// A table-checkpoint mark: the place in the log at which the sample of
/// the value log's counts was taken for a page-table checkpoint.
///
/// A mark changes nothing. The place of the sample is the end of its mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckpointMark {
    covers: CommitVersion,
}

impl CheckpointMark {
    /// The mark of a checkpoint that covers the commit version `covers`.
    pub const fn new(covers: CommitVersion) -> Self {
        Self { covers }
    }

    /// The commit version the checkpoint covers.
    pub const fn covers(&self) -> CommitVersion {
        self.covers
    }

    /// Appends the mark's record to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let mut payload = [0; CHECKPOINT_PAYLOAD_LEN];
        payload[0] = MARK;
        payload[1..].copy_from_slice(&self.covers.encode());
        write_record(&payload, out)
    }

    fn decode(mut fields: Fields<'_>, version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        fields.expect_len(MARK, CHECKPOINT_PAYLOAD_LEN - 1)?;
        let covers = CommitVersion::decode(*fields.array()?).ok_or(FormatError::MissingId)?;
        Ok(Self { covers })
    }
}

/// One record of a log file, read and checked by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record<'a> {
    /// The start record of the file.
    Start(LogStart),
    /// One record of a value that a commit group carries.
    Value(ValuePart<'a>),
    /// A carried value: a value that a commit group names where it lies in
    /// the value log.
    Carried(CarriedValue),
    /// One part of a commit group.
    Part(GroupPart<'a>),
    /// A background change.
    Change(Change<'a>),
    /// A table-checkpoint mark.
    Mark(CheckpointMark),
}

impl<'a> Record<'a> {
    /// Reads the record at the start of `bytes`, in a log file of format
    /// `version`, and returns it with its length in bytes. Later bytes are
    /// ignored.
    ///
    /// The checks run in a fixed order: the frame's length, that the whole
    /// record is there, its checksum, its kind, and then the fields of that
    /// kind. A record is checked by itself here. What may follow what is
    /// for [`LogCursor`](super::LogCursor).
    pub fn decode(bytes: &'a [u8], version: FormatVersion) -> Result<(Self, usize), FormatError> {
        Self::decode_with(bytes, version, Limits::FORMAT)
    }

    pub(crate) fn decode_with(
        bytes: &'a [u8],
        version: FormatVersion,
        limits: Limits,
    ) -> Result<(Self, usize), FormatError> {
        let header = RecordHeader::decode(bytes, limits.max_payload)?;
        let len = RecordHeader::ENCODED_LEN + header.payload_len();
        let record = bytes.get(..len).ok_or(FormatError::Truncated {
            needed: len,
            available: bytes.len(),
        })?;
        let payload = header.verified_payload(&record[RecordHeader::ENCODED_LEN..])?;
        let mut fields = Fields::new(payload, RecordHeader::ENCODED_LEN);
        let kind = fields
            .byte()
            .map_err(|_| WalError::RecordLength { kind: 0, len: 0 })?;
        let record = match kind {
            START => Self::Start(LogStart::decode(fields, version)?),
            VALUE => Self::Value(ValuePart::decode(fields, version, limits)?),
            CARRIED => Self::Carried(CarriedValue::decode(fields, version)?),
            PART => Self::Part(GroupPart::decode(fields, payload.len(), version)?),
            CHANGE => Self::Change(Change::decode(fields, version)?),
            MARK => Self::Mark(CheckpointMark::decode(fields, version)?),
            code => return Err(FormatError::UnknownRecordKind { code: code.into() }),
        };
        Ok((record, len))
    }

    /// The record's kind code.
    pub const fn kind_code(&self) -> u8 {
        match self {
            Self::Start(_) => START,
            Self::Value(_) => VALUE,
            Self::Carried(_) => CARRIED,
            Self::Part(_) => PART,
            Self::Change(_) => CHANGE,
            Self::Mark(_) => MARK,
        }
    }
}

/// The length in bytes of the record whose first bytes are `bytes`, in a
/// log file of format `version`: what a reader fetches before it calls
/// [`Record::decode`]. It needs the 8 bytes of the record's frame, and it
/// checks the length they state against the longest record.
pub fn record_len(bytes: &[u8], version: FormatVersion) -> Result<usize, FormatError> {
    match version {
        FormatVersion::V1 => {}
    }
    let header = RecordHeader::decode(bytes, Limits::FORMAT.max_payload)?;
    Ok(RecordHeader::ENCODED_LEN + header.payload_len())
}
