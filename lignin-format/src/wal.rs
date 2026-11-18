//! The write-ahead log: the files `wal/<n>.log`.
//!
//! See `docs/lignindb/log-format.md`.
//!
//! A log file is its file header, a start record, and then items: commit
//! groups, records of background changes, and checkpoint marks. Every
//! record is in the default frame, and its payload starts with its kind.
//!
//! ```text
//! code  record            payload after the kind byte
//!    1  start             base (8), after (8)
//!    2  value             owner (4), part (2), value length (4), data
//!    3  carried value     owner (4), source owner (4), source (16)
//!    4  group part        flags (1), version (8), part (4), entries
//!    5  change            what (1), then the fields of that change
//!    6  checkpoint mark   covers (8)
//! ```
//!
//! A commit group is its values, as value records and carried value
//! records, and then its parts, one record after another. An entry of a
//! part is one byte, its kind, and then:
//!
//! ```text
//! code  entry                    after the kind byte
//!    1  tree page run            length (4), the run of page deltas
//!    2  flat page run            length (4), the run of flat changes
//!    3  freed page               page id (4)
//!    4  released value pointer   the pointer (16)
//!    5  acquired value pointer   the pointer (16)
//! ```
//!
//! A change is one of three, by its first byte:
//!
//! ```text
//! what  change                   after that byte
//!    1  value of a tree cell     page (4), slot (2), from (16), to (16)
//!    2  value of a flat entry    page (4), from (16), to (16)
//!    3  reclaim                  flags (1), page (4), version (8) if the
//!                                page is freed, count (2), that many
//!                                released pointers (16 each), a run
//! ```
//!
//! A record of a flat run is one byte, its kind, and then:
//!
//! ```text
//! code  record        after the kind byte
//!    1  image         length (2), an image of the page
//!    2  insert        key length (2), key, flags (1), length (2), value
//!    3  replace       key length (2), key, flags (1), length (2), value
//!    4  remove        key length (2), key
//!    5  remove range  key length (2), key, count (2)
//!    6  insert child  key length (2), separator, child page id (4)
//!    7  set prefix    length (2), prefix
//! ```
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     delta::Delta,
//!     tree_page::HashField,
//!     wal::{self, Entry, GroupWriter, LogAddress, LogItem, LogStart, LoggedValue},
//!     CommitVersion, FileId, PageId, SlotId,
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let file = FileId::new(45).expect("not zero");
//! let version = CommitVersion::new(1).expect("a version");
//! let page = PageId::new(7).expect("not zero");
//!
//! // A new log file: its header and its start record.
//! let start = LogStart::FIRST;
//! let mut log = Vec::new();
//! wal::write_file_start(file, &start, &mut log)?;
//!
//! // The commit of version 1 writes a value that is too long for its
//! // cell, and changes one hash of page 7.
//! let at = start.address(log.len() as u64)?;
//! let mut group = GroupWriter::new(version, at, &mut log)?;
//! let pending = group.value(page, b"a value too long for its cell")?;
//! group.tree_run_with(page, None, |run| {
//!     run.push(&Delta::Hash {
//!         slot: SlotId::MIN,
//!         field: HashField::KeyValue,
//!         hash: &[0x55; 32],
//!     })
//! })?;
//! group.finish();
//!
//! // What the cell holds for the value is the place of the value's record
//! // in the log.
//! let address = LogAddress::of(&pending)?;
//! assert_eq!(start.offset_of(address), Some(wal::FIRST_ITEM_OFFSET));
//!
//! // A reader gets the group back, and nothing of a torn tail.
//! let whole = log.len() as u64;
//! log.extend_from_slice(&[0; 16]);
//! let mut scan = wal::scan(&log)?;
//! let Some(LogItem::Commit(group)) = scan.next() else {
//!     panic!("a commit group");
//! };
//! assert_eq!(group.version(), version);
//! let (at, value) = group.values().next().expect("the value");
//! assert_eq!(at, address);
//! assert!(matches!(value, LoggedValue::Bytes(bytes) if bytes.owner() == page));
//! assert!(matches!(group.entries().next(), Some(Entry::Tree(run)) if run.page() == page));
//! assert!(scan.next().is_none());
//! assert_eq!(scan.tail().valid_len(), whole);
//! assert!(scan.tail().failure().is_some());
//! # Ok(())
//! # }
//! ```

use std::fmt;

use crate::{
    checksum::MAX_CHECKSUMMED_LEN,
    error::FormatError,
    header::{FileHeader, FileKind, FormatVersion, RecordHeader},
    ids::{CommitVersion, FileId, PageId},
};

mod change;
mod cursor;
mod flat;
mod group;
mod place;
mod record;
mod scan;
mod value;

#[cfg(test)]
mod tests;

pub use change::{write_reclaim, Change, Reclaim, Released, Removed, ValueMove};
pub use cursor::{LogCursor, Step};
pub use flat::{FlatChange, FlatChanges, FlatRun, FlatRunWriter};
pub use group::{Entries, Entry, Group, GroupPart, GroupValues, GroupWriter};
pub use place::{LogAddress, LogPlace, LogStart};
pub use record::{record_len, CheckpointMark, Record};
pub use scan::{scan, LogItem, LogScan, LogTail, TailFailure};
pub use value::{
    logged_extent_len, logged_read_len, read_logged_value, read_pending_value, CarriedValue,
    LoggedBytes, LoggedParts, LoggedValue, ValuePart,
};

/// Bytes of the start record of a log file, frame included.
pub const START_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + record::START_PAYLOAD_LEN;

/// The offset of the first item of a log file: behind the file header and
/// the start record.
pub const FIRST_ITEM_OFFSET: u64 = (FileHeader::ENCODED_LEN + START_RECORD_LEN) as u64;

/// The largest offset in a log file, which is the largest a file API takes.
pub const MAX_LOG_OFFSET: u64 = i64::MAX as u64;

/// The longest payload of a log record.
pub const MAX_RECORD_PAYLOAD_LEN: usize = RecordHeader::MAX_PAYLOAD_LEN;

/// Bytes of a value record before the value's bytes, frame included.
pub const VALUE_RECORD_HEADER_LEN: usize = RecordHeader::ENCODED_LEN + value::VALUE_FIELDS_LEN;

/// The most bytes of a value one record carries.
pub const MAX_VALUE_PART_LEN: usize = MAX_RECORD_PAYLOAD_LEN - value::VALUE_FIELDS_LEN;

/// Bytes of a carried value record, frame included.
pub const CARRIED_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + value::CARRIED_PAYLOAD_LEN;

/// Bytes of a group part before its entries, frame included.
pub const PART_HEADER_LEN: usize = RecordHeader::ENCODED_LEN + group::PART_FIELDS_LEN;

/// The longest run an entry of a group carries.
pub const MAX_RUN_LEN: usize =
    MAX_RECORD_PAYLOAD_LEN - group::PART_FIELDS_LEN - group::RUN_ENTRY_LEN;

/// Bytes of a checkpoint mark, frame included.
pub const CHECKPOINT_MARK_LEN: usize = RecordHeader::ENCODED_LEN + record::CHECKPOINT_PAYLOAD_LEN;

const _: () =
    assert!(RecordHeader::ENCODED_LEN + MAX_RECORD_PAYLOAD_LEN == MAX_CHECKSUMMED_LEN + 4);

/// How long a record may be. Tests cut records much shorter than the format
/// does, with the same code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) max_payload: usize,
}

impl Limits {
    pub(crate) const FORMAT: Self = Self {
        max_payload: MAX_RECORD_PAYLOAD_LEN,
    };

    pub(crate) const fn max_value_part(self) -> usize {
        self.max_payload - value::VALUE_FIELDS_LEN
    }

    pub(crate) const fn max_entries(self) -> usize {
        self.max_payload - group::PART_FIELDS_LEN
    }
}

/// Why bytes are not a log, or why a log cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalError {
    /// The file header is valid but is not a log's.
    NotALog {
        /// The kind of file the header names.
        found: FileKind,
    },
    /// A log file does not begin with a start record.
    MissingStart,
    /// A start record that is not the first record of its file.
    MisplacedStart,
    /// A start record whose base lies past the largest offset.
    InvalidBase {
        /// The base.
        base: u64,
    },
    /// A start record that is not the one the file before it asks for.
    NotTheNextFile {
        /// The base the next file must have.
        expected_base: u64,
        /// The base found.
        found_base: u64,
        /// The version the next file must come after.
        expected_after: CommitVersion,
        /// The version found.
        found_after: CommitVersion,
    },
    /// An offset that is no place in a log file: before the first item or
    /// past the largest offset.
    InvalidPlace {
        /// The offset.
        offset: u64,
    },
    /// A log address past the end of what a log can hold.
    AddressOutOfRange,
    /// A record's payload is not of a length its kind has. An empty
    /// payload, which has no kind, is kind 0.
    RecordLength {
        /// The record kind code.
        kind: u8,
        /// The payload length.
        len: usize,
    },
    /// A value of that length has no part with that number.
    NoSuchValuePart {
        /// The part number in the record.
        part: u16,
        /// The value length in the record.
        value_len: u32,
    },
    /// A value record is not the part of its value that belongs at its
    /// place.
    UnexpectedValuePart {
        /// The part that belongs there.
        expected: u16,
        /// The part number in the record.
        found: u16,
    },
    /// A value record does not carry its part's number of bytes.
    WrongValuePartLength {
        /// The bytes that part of the value has.
        expected: usize,
        /// The bytes the record carries.
        found: usize,
    },
    /// A later part of a value names another owner or value length than
    /// the first.
    PartOfAnotherValue,
    /// The records of a value end before its last part.
    UnfinishedValue,
    /// What stands at the address of a pending value is neither a value
    /// nor a carried value.
    NotAValue {
        /// The kind code of the record found.
        kind: u8,
    },
    /// The value at the address of a pending value is not of the pending
    /// value's length.
    WrongValueLength {
        /// The length the pending value states.
        expected: u32,
        /// The length the record states.
        found: u32,
    },
    /// The value at the address of a pending value belongs to a cell of
    /// another page.
    WrongOwner {
        /// The page whose cell holds the pending value.
        expected: PageId,
        /// The owner the record names.
        found: PageId,
    },
    /// Version 0 where the version of a commit must stand: no commit has
    /// it.
    NotACommit,
    /// The version of a commit group is not the one after the group
    /// before it.
    VersionGap {
        /// The version of the group before it.
        after: CommitVersion,
        /// The version found.
        found: CommitVersion,
    },
    /// The parts of a group do not come in order from 0.
    UnexpectedPart {
        /// The part that belongs there.
        expected: u32,
        /// The part number in the record.
        found: u32,
    },
    /// A part of another commit than the group it stands in.
    PartOfAnotherGroup,
    /// A record that may not stand inside a commit group.
    GroupInterrupted {
        /// The record kind code.
        kind: u8,
    },
    /// The log ends inside a commit group.
    UnfinishedGroup,
    /// A part with no entry that is not the only part of its group.
    EmptyPart,
    /// A part that is followed by another although the next entry would
    /// have fitted into it.
    PartNotFull,
    /// A group of more parts than can be numbered.
    TooManyParts,
    /// Entries that are not in their order, or one that is there twice.
    EntriesOutOfOrder,
    /// A value that is written into a group behind its first entry.
    ValueAfterEntries,
    /// A run is too long for one record.
    RunTooLong {
        /// The run's length.
        len: usize,
        /// The longest run a record carries.
        max: usize,
    },
    /// A background change that no page step makes: a value that moves to
    /// the place it has or to a value of another length, or a reclaiming
    /// step that does more than remove, or that neither removes nor frees.
    InvalidChange,
    /// A page is freed with a version that no commit before the record
    /// has.
    FreedAhead {
        /// The version the page is freed with.
        version: CommitVersion,
        /// The newest version a commit before the record has.
        latest: CommitVersion,
    },
    /// A checkpoint mark covers a version that no commit before it has.
    CheckpointAhead {
        /// The version the mark covers.
        covers: CommitVersion,
        /// The newest version a commit before the mark has.
        latest: CommitVersion,
    },
    /// A flat run holds no record.
    EmptyFlatRun,
    /// An image that is not the first record of its flat run.
    MisplacedImage,
    /// An image in a flat run is not as long as it says.
    ImageLength {
        /// The length the image states.
        stated: usize,
        /// The bytes the record carries.
        found: usize,
    },
    /// A key, a value or a prefix is too long for a record of a flat run.
    FlatFieldTooLong {
        /// Its length.
        len: usize,
    },
    /// The place of a value out of line is not 16 bytes long.
    PlaceLength {
        /// Its length.
        len: usize,
    },
    /// A range of fewer than two entries.
    InvalidRange,
    /// A flat run that does not start from an image was given no page.
    NoPage,
    /// A run or a flat page image is of another page than the one it
    /// stands for: the run of a reclaiming step, the image in a flat run,
    /// the image a flat run or a value change is applied to.
    WrongPage {
        /// The page the run names, or the page the image is of.
        found: PageId,
        /// The page it stands for.
        expected: PageId,
    },
    /// A change for a leaf was applied to a branch, or the other way
    /// round.
    WrongPageKind,
    /// A key that must be in the page is not.
    KeyAbsent,
    /// A key that must not be in the page is.
    KeyPresent,
    /// Fewer entries follow a key than a range takes.
    RangeTooLong,
    /// No entry of the page holds the place a value is moved from.
    NoSuchPlace,
}

impl fmt::Display for WalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALog { found } => write!(f, "a {found:?} file, not a log"),
            Self::MissingStart => write!(f, "a log file that does not begin with a start record"),
            Self::MisplacedStart => write!(f, "a start record that is not the first record"),
            Self::InvalidBase { base } => write!(f, "a log file that starts at address {base}"),
            Self::NotTheNextFile {
                expected_base,
                found_base,
                expected_after,
                found_after,
            } => write!(
                f,
                "a log file that starts at address {found_base} after version {found_after}, \
                 where the log before it ends at {expected_base} after version {expected_after}"
            ),
            Self::InvalidPlace { offset } => write!(f, "offset {offset} is no place in a log file"),
            Self::AddressOutOfRange => write!(f, "a log address past the end of the log"),
            Self::RecordLength { kind, len } => {
                write!(f, "a record of kind {kind} with a payload of {len} bytes")
            }
            Self::NoSuchValuePart { part, value_len } => {
                write!(f, "part {part} of a value of {value_len} bytes")
            }
            Self::UnexpectedValuePart { expected, found } => {
                write!(f, "part {found} of a value where part {expected} belongs")
            }
            Self::WrongValuePartLength { expected, found } => write!(
                f,
                "a part of a value with {found} bytes, where the part has {expected}"
            ),
            Self::PartOfAnotherValue => write!(f, "a part of another value inside a value"),
            Self::UnfinishedValue => write!(f, "a value whose last part is missing"),
            Self::NotAValue { kind } => {
                write!(f, "a record of kind {kind} where a value must stand")
            }
            Self::WrongValueLength { expected, found } => write!(
                f,
                "a value of {found} bytes where the pending value states {expected}"
            ),
            Self::WrongOwner { expected, found } => write!(
                f,
                "a value owned by page {found}, read for a cell of page {expected}"
            ),
            Self::NotACommit => write!(f, "version 0 where the version of a commit must stand"),
            Self::VersionGap { after, found } => write!(
                f,
                "a commit group of version {found} after the group of version {after}"
            ),
            Self::UnexpectedPart { expected, found } => {
                write!(f, "part {found} of a group where part {expected} belongs")
            }
            Self::PartOfAnotherGroup => write!(f, "a part of another commit inside a group"),
            Self::GroupInterrupted { kind } => {
                write!(f, "a record of kind {kind} inside a commit group")
            }
            Self::UnfinishedGroup => write!(f, "the log ends inside a commit group"),
            Self::EmptyPart => write!(f, "a part of a group with no entry"),
            Self::PartNotFull => write!(f, "a part of a group that the next entry fits into"),
            Self::TooManyParts => write!(f, "a group of more parts than can be numbered"),
            Self::EntriesOutOfOrder => write!(f, "entries out of order"),
            Self::ValueAfterEntries => write!(f, "a value behind the first entry of its group"),
            Self::RunTooLong { len, max } => {
                write!(f, "a run of {len} bytes exceeds the maximum of {max}")
            }
            Self::InvalidChange => write!(f, "a background change that no page step makes"),
            Self::FreedAhead { version, latest } => write!(
                f,
                "a page freed with version {version}, where the latest commit is {latest}"
            ),
            Self::CheckpointAhead { covers, latest } => write!(
                f,
                "a checkpoint mark that covers version {covers}, where the latest commit is {latest}"
            ),
            Self::EmptyFlatRun => write!(f, "a flat run with no record"),
            Self::MisplacedImage => {
                write!(f, "an image that is not the first record of its flat run")
            }
            Self::ImageLength { stated, found } => write!(
                f,
                "an image of {found} bytes that states a length of {stated}"
            ),
            Self::FlatFieldTooLong { len } => {
                write!(f, "a field of {len} bytes in a record of a flat run")
            }
            Self::PlaceLength { len } => {
                write!(f, "the place of a value out of line in {len} bytes")
            }
            Self::InvalidRange => write!(f, "a range of fewer than two entries"),
            Self::NoPage => write!(f, "a flat run that starts from no image, and no page"),
            Self::WrongPage { found, expected } => {
                write!(
                    f,
                    "a run or an image of page {found} where one of page {expected} belongs"
                )
            }
            Self::WrongPageKind => write!(f, "a change for the other kind of flat page"),
            Self::KeyAbsent => write!(f, "a key that must be in the page is not"),
            Self::KeyPresent => write!(f, "a key that must not be in the page is"),
            Self::RangeTooLong => write!(f, "a range that runs past the last entry"),
            Self::NoSuchPlace => write!(f, "no entry holds the place the value is moved from"),
        }
    }
}

impl std::error::Error for WalError {}

impl From<WalError> for FormatError {
    fn from(error: WalError) -> Self {
        Self::Wal(error)
    }
}

/// Appends the first bytes of the log file `file` to `out`: its file header
/// and its start record.
pub fn write_file_start(
    file: FileId,
    start: &LogStart,
    out: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let header = FileHeader::new(FileKind::Wal, FormatVersion::CURRENT, file.get())?;
    out.extend_from_slice(&header.encode());
    start.write(out)
}

/// Reads the first bytes of a log file: its file header and its start
/// record. Later bytes are ignored.
pub fn read_file_start(file: &[u8]) -> Result<(FileHeader, LogStart), FormatError> {
    let header = FileHeader::decode(file)?;
    if header.kind() != FileKind::Wal {
        return Err(WalError::NotALog {
            found: header.kind(),
        }
        .into());
    }
    let records = file.get(FileHeader::ENCODED_LEN..).unwrap_or_default();
    let start = match Record::decode(records, header.version()) {
        Ok((Record::Start(start), _)) => start,
        Ok(_) => return Err(WalError::MissingStart.into()),
        Err(error) => return Err(record::rebase(error, FileHeader::ENCODED_LEN)),
    };
    Ok((header, start))
}
