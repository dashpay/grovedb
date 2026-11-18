//! What may follow what in a log file: the reader that goes through a file
//! record by record and knows where the last whole item ends.

use super::{
    change::Change,
    group::{GroupPart, OrderKey},
    place::LogStart,
    record::Record,
    value::ValuePart,
    Limits, WalError, FIRST_ITEM_OFFSET, MAX_LOG_OFFSET,
};
use crate::{
    error::FormatError,
    header::{FileHeader, FileKind, FormatVersion},
    ids::{CommitVersion, PageId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between two items.
    Idle,
    /// Inside the records of a value of a group.
    Value {
        owner: PageId,
        value_len: u32,
        next: u16,
    },
    /// Among the values of a group, between two values.
    Values,
    /// Among the parts of a group.
    Parts {
        commit: CommitVersion,
        next: u32,
        payload_len: usize,
        last_key: Option<OrderKey>,
    },
}

/// One record that a [`LogCursor`] has read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step<'a> {
    /// The record.
    pub record: Record<'a>,
    /// The record's length in bytes.
    pub len: usize,
    /// Whether the record ends an item: it is the last part of a commit
    /// group, a background change or a checkpoint mark. Until its last
    /// part is read, nothing of a group is taken.
    pub ends_item: bool,
}

/// Reads the records of one log file in order, checks that each may stand
/// where it does, and knows where the last whole item ends.
///
/// An item is a commit group, the record of a background change, or a
/// checkpoint mark. A reader that does not hold the whole file in memory
/// fetches the next record with [`record_len`](super::record_len) and
/// hands it to [`Self::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCursor {
    version: FormatVersion,
    limits: Limits,
    offset: u64,
    item_at: u64,
    latest: CommitVersion,
    state: State,
}

impl LogCursor {
    /// A cursor at the first item of the log file with the header `header`
    /// and the start record `start`.
    pub fn new(header: &FileHeader, start: &LogStart) -> Result<Self, FormatError> {
        Self::with_limits(header, start, Limits::FORMAT)
    }

    pub(crate) fn with_limits(
        header: &FileHeader,
        start: &LogStart,
        limits: Limits,
    ) -> Result<Self, FormatError> {
        if header.kind() != FileKind::Wal {
            return Err(WalError::NotALog {
                found: header.kind(),
            }
            .into());
        }
        Ok(Self {
            version: header.version(),
            limits,
            offset: FIRST_ITEM_OFFSET,
            item_at: FIRST_ITEM_OFFSET,
            latest: start.after(),
            state: State::Idle,
        })
    }

    /// The offset in the file of the record that [`Self::step`] reads
    /// next.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// The offset at which the last whole item ends: the length of the
    /// longest prefix of the file, as far as it was read, that holds whole
    /// items only.
    pub const fn valid_len(&self) -> u64 {
        match self.state {
            State::Idle => self.offset,
            _ => self.item_at,
        }
    }

    /// The offset at which the item begins that the cursor is in, or that
    /// the next record begins.
    pub const fn item_offset(&self) -> u64 {
        self.valid_len()
    }

    /// The version of the last whole commit group, or of the last group
    /// before the file if it has read none.
    pub const fn latest(&self) -> CommitVersion {
        self.latest
    }

    /// Whether the cursor is inside a commit group: it has read some of
    /// the group's records and not its last part.
    pub const fn in_group(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Reads the record at the start of `bytes`, which are the bytes of
    /// the file from [`Self::offset`] on, and moves past it. Later bytes
    /// are ignored.
    ///
    /// The record is checked by itself, as [`Record::decode`] checks it,
    /// and then against what came before it. On an error the cursor has
    /// not moved, and nothing from its offset on is part of the log.
    pub fn step<'a>(&mut self, bytes: &'a [u8]) -> Result<Step<'a>, FormatError> {
        let (record, len) = Record::decode_with(bytes, self.version, self.limits)?;
        let state = self.after(&record)?;
        let offset = self
            .offset
            .checked_add(len as u64)
            .filter(|offset| *offset <= MAX_LOG_OFFSET)
            .ok_or(WalError::InvalidPlace {
                offset: self.offset,
            })?;
        if self.state == State::Idle {
            self.item_at = self.offset;
        }
        self.offset = offset;
        self.state = state;
        let ends_item = state == State::Idle;
        if let (true, Record::Part(part)) = (ends_item, &record) {
            self.latest = part.version();
        }
        Ok(Step {
            record,
            len,
            ends_item,
        })
    }

    /// Checks that the file may end where the cursor is: not inside a
    /// commit group.
    pub fn finish(&self) -> Result<(), FormatError> {
        if self.in_group() {
            return Err(WalError::UnfinishedGroup.into());
        }
        Ok(())
    }

    fn after(&self, record: &Record<'_>) -> Result<State, FormatError> {
        match (record, self.state) {
            (Record::Start(_), _) => Err(WalError::MisplacedStart.into()),
            (Record::Value(part), State::Idle | State::Values) => {
                part.check_continues(part.owner(), part.value_len(), 0)?;
                Ok(Self::in_value(part, 1))
            }
            (
                Record::Value(part),
                State::Value {
                    owner,
                    value_len,
                    next,
                },
            ) => {
                part.check_continues(owner, value_len, next)?;
                Ok(Self::in_value(part, next.saturating_add(1)))
            }
            (_, State::Value { .. }) => Err(WalError::UnfinishedValue.into()),
            (Record::Carried(_), State::Idle | State::Values) => Ok(State::Values),
            (Record::Part(part), State::Idle | State::Values) => {
                let expected = self.latest.next();
                if expected != Some(part.version()) {
                    return Err(WalError::VersionGap {
                        after: self.latest,
                        found: part.version(),
                    }
                    .into());
                }
                self.in_parts(part, 0, None)
            }
            (
                Record::Part(part),
                State::Parts {
                    commit,
                    next,
                    payload_len,
                    last_key,
                },
            ) => {
                if part.version() != commit {
                    return Err(WalError::PartOfAnotherGroup.into());
                }
                self.in_parts(part, next, Some((payload_len, last_key)))
            }
            (record, State::Values | State::Parts { .. }) => Err(WalError::GroupInterrupted {
                kind: record.kind_code(),
            }
            .into()),
            (Record::Change(change), State::Idle) => {
                self.check_change(change)?;
                Ok(State::Idle)
            }
            (Record::Mark(mark), State::Idle) => {
                if mark.covers() > self.latest {
                    return Err(WalError::CheckpointAhead {
                        covers: mark.covers(),
                        latest: self.latest,
                    }
                    .into());
                }
                Ok(State::Idle)
            }
        }
    }

    const fn in_value(part: &ValuePart<'_>, next: u16) -> State {
        if part.is_last() {
            State::Values
        } else {
            State::Value {
                owner: part.owner(),
                value_len: part.value_len(),
                next,
            }
        }
    }

    fn in_parts(
        &self,
        part: &GroupPart<'_>,
        expected: u32,
        before: Option<(usize, Option<OrderKey>)>,
    ) -> Result<State, FormatError> {
        if part.part() != expected {
            return Err(WalError::UnexpectedPart {
                expected,
                found: part.part(),
            }
            .into());
        }
        if let Some((payload_len, last_key)) = before {
            let (first_key, first_len) = part.first().ok_or(WalError::EmptyPart)?;
            if payload_len + first_len <= self.limits.max_payload {
                return Err(WalError::PartNotFull.into());
            }
            if last_key.is_some_and(|last_key| first_key <= last_key) {
                return Err(WalError::EntriesOutOfOrder.into());
            }
        }
        if part.is_last() {
            return Ok(State::Idle);
        }
        Ok(State::Parts {
            commit: part.version(),
            next: expected.checked_add(1).ok_or(WalError::TooManyParts)?,
            payload_len: part.payload_len(),
            last_key: part.last_key(),
        })
    }

    fn check_change(&self, change: &Change<'_>) -> Result<(), FormatError> {
        let Change::Reclaim(reclaim) = change else {
            return Ok(());
        };
        match reclaim.freed() {
            Some(version) if version > self.latest => Err(WalError::FreedAhead {
                version,
                latest: self.latest,
            }
            .into()),
            _ => Ok(()),
        }
    }
}
