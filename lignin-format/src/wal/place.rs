//! Places in the log: a file and an offset, a log address, and the start
//! record that ties the two together.

use std::{fmt, num::NonZeroU64};

use super::{
    record::{Fields, START, START_PAYLOAD_LEN},
    WalError, FIRST_ITEM_OFFSET, MAX_LOG_OFFSET,
};
use crate::{
    error::FormatError,
    header::{write_record, FormatVersion},
    ids::{CommitVersion, FileId},
    value_log::PendingValue,
};

const fn check_offset(offset: u64) -> Result<(), FormatError> {
    if offset < FIRST_ITEM_OFFSET || offset > MAX_LOG_OFFSET {
        return Err(FormatError::Wal(WalError::InvalidPlace { offset }));
    }
    Ok(())
}

/// A place in the log: a log file and an offset in it, at or after the
/// first item of the file. It is where one item ends and the next begins,
/// or the end of the file.
///
/// Places are in the order of the log: by file, then by offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogPlace {
    file: FileId,
    offset: u64,
}

impl LogPlace {
    /// The place at `offset` of the log file `file`.
    ///
    /// The offset is at or after [`FIRST_ITEM_OFFSET`], behind the file
    /// header and the start record, and at most [`MAX_LOG_OFFSET`].
    /// Anything else is [`WalError::InvalidPlace`].
    pub const fn new(file: FileId, offset: u64) -> Result<Self, FormatError> {
        match check_offset(offset) {
            Ok(()) => Ok(Self { file, offset }),
            Err(error) => Err(error),
        }
    }

    /// The log file.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The offset in the log file.
    pub const fn offset(&self) -> u64 {
        self.offset
    }
}

/// A position in the log as one number: the base of a log file plus an
/// offset in it. It is what the 8 bytes of a pending value hold.
///
/// An address is never 0, and it names the same byte for as long as the
/// log file that holds the byte is kept.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogAddress(NonZeroU64);

impl LogAddress {
    /// Bytes an address takes on disk.
    pub const ENCODED_LEN: usize = PendingValue::ID_LEN;

    /// The address with this number, or `None` for zero, which is not an
    /// address.
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// The address's number.
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The eight bytes written to disk.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.get().to_le_bytes()
    }

    /// Reads an address field. Zero decodes to `None`.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Option<Self> {
        Self::new(u64::from_le_bytes(bytes))
    }

    /// The address a pending value holds: where the first record of the
    /// value is in the log.
    pub const fn of(pending: &PendingValue) -> Result<Self, FormatError> {
        match Self::decode(pending.id()) {
            Some(address) => Ok(address),
            None => Err(FormatError::MissingId),
        }
    }

    /// The pending value of `value_len` bytes whose first record is at this
    /// address: what its cell holds until the value has a place in the
    /// value log.
    pub const fn pending(self, value_len: u32) -> Result<PendingValue, FormatError> {
        PendingValue::new(self.encode(), value_len)
    }

    /// The address `len` bytes further on.
    pub const fn after(self, len: u64) -> Result<Self, FormatError> {
        match self.0.checked_add(len) {
            Some(raw) => Ok(Self(raw)),
            None => Err(FormatError::Wal(WalError::AddressOutOfRange)),
        }
    }
}

impl fmt::Debug for LogAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "log@{}", self.get())
    }
}

impl fmt::Display for LogAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), f)
    }
}

/// The start record of a log file: where the file begins in the log, and
/// which commit it comes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogStart {
    base: u64,
    after: CommitVersion,
}

impl LogStart {
    /// The start record of the first log file of a database.
    pub const FIRST: Self = Self {
        base: 0,
        after: CommitVersion::EMPTY,
    };

    /// The start record of a log file whose first byte has the log address
    /// `base`, and which comes after the commit group of version `after`:
    /// the first group in the file has the next version. `after` is
    /// [`CommitVersion::EMPTY`] when no commit is before the file.
    ///
    /// A base above [`MAX_LOG_OFFSET`] is [`WalError::InvalidBase`].
    pub const fn new(base: u64, after: CommitVersion) -> Result<Self, FormatError> {
        if base > MAX_LOG_OFFSET {
            return Err(FormatError::Wal(WalError::InvalidBase { base }));
        }
        Ok(Self { base, after })
    }

    /// The log address of the file's first byte.
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// The version of the last commit group before the file.
    pub const fn after(&self) -> CommitVersion {
        self.after
    }

    /// The log address of the byte at `offset` of the file, which is at or
    /// after the first item.
    pub const fn address(&self, offset: u64) -> Result<LogAddress, FormatError> {
        if let Err(error) = check_offset(offset) {
            return Err(error);
        }
        match LogAddress::new(self.base + offset) {
            Some(address) => Ok(address),
            None => Err(FormatError::Wal(WalError::AddressOutOfRange)),
        }
    }

    /// The offset in this file of the byte at `address`, or `None` when the
    /// address is before the file's first item. Whether the file is that
    /// long is for the caller to check.
    pub const fn offset_of(&self, address: LogAddress) -> Option<u64> {
        match address.get().checked_sub(self.base) {
            Some(offset) if check_offset(offset).is_ok() => Some(offset),
            _ => None,
        }
    }

    /// The start record of the log file that comes next, when this file is
    /// valid up to `valid_len` and the last commit group of the log up to
    /// there has the version `after`.
    pub const fn next(&self, valid_len: u64, after: CommitVersion) -> Result<Self, FormatError> {
        if let Err(error) = check_offset(valid_len) {
            return Err(error);
        }
        Self::new(self.base + valid_len, after)
    }

    /// Checks that `found`, the start record of another log file, is the
    /// one [`Self::next`] gives: that file continues the log where this one
    /// stops being valid.
    pub fn check_next(
        &self,
        valid_len: u64,
        after: CommitVersion,
        found: &Self,
    ) -> Result<(), FormatError> {
        let expected = self.next(valid_len, after)?;
        if *found != expected {
            return Err(WalError::NotTheNextFile {
                expected_base: expected.base,
                found_base: found.base,
                expected_after: expected.after,
                found_after: found.after,
            }
            .into());
        }
        Ok(())
    }

    /// Appends the start record to `out`.
    pub(super) fn write(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let mut payload = [0; START_PAYLOAD_LEN];
        payload[0] = START;
        payload[1..9].copy_from_slice(&self.base.to_le_bytes());
        payload[9..].copy_from_slice(&self.after.encode());
        write_record(&payload, out)
    }

    pub(super) fn decode(
        mut fields: Fields<'_>,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        fields.expect_len(START, START_PAYLOAD_LEN - 1)?;
        let base = u64::from_le_bytes(*fields.array()?);
        let after = CommitVersion::decode(*fields.array()?).ok_or(FormatError::MissingId)?;
        Self::new(base, after)
    }
}
