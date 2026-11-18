//! Reading a whole log file: its items, and where its valid part ends.

use std::iter::FusedIterator;

use super::{
    change::Change,
    cursor::LogCursor,
    group::Group,
    place::LogStart,
    read_file_start,
    record::{CheckpointMark, Record},
    Limits, WalError,
};
use crate::{error::FormatError, header::FileHeader, ids::CommitVersion};

/// One item of a log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogItem<'a> {
    /// The group of a commit.
    Commit(Group<'a>),
    /// A background change.
    Change(Change<'a>),
    /// A table-checkpoint mark.
    Mark(CheckpointMark),
}

/// Why a scan stopped before the end of the bytes it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailFailure {
    /// The offset in the file of the record that failed, or of the end of
    /// the bytes if they end inside a commit group.
    pub offset: u64,
    /// What is wrong there. A count of bytes in it starts at `offset`.
    pub error: FormatError,
}

/// Where the valid part of a log file ends, and what follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogTail {
    valid_len: u64,
    latest: CommitVersion,
    failure: Option<TailFailure>,
}

impl LogTail {
    /// The length of the longest prefix of the file that holds whole items
    /// only: the end of the last whole commit group, background change or
    /// checkpoint mark, or the offset of the first item if there is none.
    pub const fn valid_len(&self) -> u64 {
        self.valid_len
    }

    /// The version of the last whole commit group of the log up to
    /// [`Self::valid_len`]: the last one in the file, or the last one
    /// before the file if it holds none.
    pub const fn latest(&self) -> CommitVersion {
        self.latest
    }

    /// Why the bytes after [`Self::valid_len`] are not part of the log, or
    /// `None` if there are none.
    pub const fn failure(&self) -> Option<TailFailure> {
        self.failure
    }
}

/// The items of a log file, in order, up to the first that is not whole.
/// Returned by [`scan`].
///
/// When the iteration has ended, [`Self::tail`] says where the valid part
/// of the file ends.
#[derive(Debug, Clone)]
pub struct LogScan<'a> {
    file: &'a [u8],
    header: FileHeader,
    start: LogStart,
    cursor: LogCursor,
    limits: Limits,
    failure: Option<TailFailure>,
    done: bool,
}

impl<'a> LogScan<'a> {
    /// The file's header.
    pub const fn header(&self) -> FileHeader {
        self.header
    }

    /// The file's start record.
    pub const fn start(&self) -> LogStart {
        self.start
    }

    /// The offset at which the next item starts: the end of the item that
    /// was returned last.
    pub const fn offset(&self) -> u64 {
        self.cursor.valid_len()
    }

    /// Where the valid part of the file ends, as far as the scan has
    /// read. It is final once the iteration has ended.
    pub const fn tail(&self) -> LogTail {
        LogTail {
            valid_len: self.cursor.valid_len(),
            latest: self.cursor.latest(),
            failure: self.failure,
        }
    }

    fn rest(&self) -> &'a [u8] {
        usize::try_from(self.cursor.offset())
            .ok()
            .and_then(|at| self.file.get(at..))
            .unwrap_or_default()
    }

    fn item(
        &self,
        record: Record<'a>,
        item_at: u64,
        parts_at: u64,
    ) -> Result<LogItem<'a>, FormatError> {
        let part = match record {
            Record::Change(change) => return Ok(LogItem::Change(change)),
            Record::Mark(mark) => return Ok(LogItem::Mark(mark)),
            Record::Part(part) => part,
            other => {
                return Err(WalError::GroupInterrupted {
                    kind: other.kind_code(),
                }
                .into())
            }
        };
        let bytes = self
            .file
            .get(item_at as usize..self.cursor.offset() as usize)
            .unwrap_or_default();
        Ok(LogItem::Commit(Group::new(
            bytes,
            (parts_at - item_at) as usize,
            part.version(),
            self.start.address(item_at)?,
            self.header.version(),
            self.limits,
        )))
    }

    fn read_item(&mut self) -> Result<Option<LogItem<'a>>, (u64, FormatError)> {
        let item_at = self.cursor.offset();
        let mut parts_at = None;
        loop {
            let offset = self.cursor.offset();
            let rest = self.rest();
            if rest.is_empty() {
                return match self.cursor.finish() {
                    Ok(()) => Ok(None),
                    Err(error) => Err((offset, error)),
                };
            }
            let step = self.cursor.step(rest).map_err(|error| (offset, error))?;
            if matches!(step.record, Record::Part(_)) {
                parts_at = parts_at.or(Some(offset));
            }
            if step.ends_item {
                return self
                    .item(step.record, item_at, parts_at.unwrap_or(item_at))
                    .map(Some)
                    .map_err(|error| (item_at, error));
            }
        }
    }
}

impl<'a> Iterator for LogScan<'a> {
    type Item = LogItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.read_item() {
            Ok(Some(item)) => return Some(item),
            Ok(None) => {}
            Err((offset, error)) => self.failure = Some(TailFailure { offset, error }),
        }
        self.done = true;
        None
    }
}

impl FusedIterator for LogScan<'_> {}

/// Reads the log file whose bytes are `file`, as far as they are whole.
///
/// It fails if the file does not begin with the header of a log file and a
/// start record: such a file holds nothing. Otherwise it gives the items
/// of the file in order and stops before the first one that is not whole:
/// at a record that is cut short, that fails its checksum or that may not
/// stand where it does, and at a commit group whose last part is missing.
/// What it has returned by then is the longest prefix of the file that
/// holds whole items only, and [`LogScan::tail`] says where it ends.
pub fn scan(file: &[u8]) -> Result<LogScan<'_>, FormatError> {
    scan_with(file, Limits::FORMAT)
}

pub(crate) fn scan_with(file: &[u8], limits: Limits) -> Result<LogScan<'_>, FormatError> {
    let (header, start) = read_file_start(file)?;
    Ok(LogScan {
        file,
        header,
        start,
        cursor: LogCursor::with_limits(&header, &start, limits)?,
        limits,
        failure: None,
        done: false,
    })
}
